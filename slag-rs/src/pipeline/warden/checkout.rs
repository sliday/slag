use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::engine::sandbox::git_async;
use crate::error::SlagError;

pub(super) struct FreshCheckout {
    _temp: tempfile::TempDir,
    pub command: String,
}

pub(super) async fn prepare(root: &Path, suite: &str) -> Result<FreshCheckout, SlagError> {
    let root = root.canonicalize()?;
    let temp = tempfile::Builder::new().prefix("slag-review-clone-").tempdir()?;
    let checkout = temp.path().join("checkout");
    let mut clone = git_async();
    clone.args(["clone", "--quiet", "--no-hardlinks", "--no-local", "--"])
        .arg(&root).arg(&checkout).current_dir(temp.path()).kill_on_drop(true);
    checked(&mut clone, "fresh checkout").await?;

    let mut paths = clean_paths(&root);
    let words = shell_words::split(suite).map_err(|error| SlagError::Config(error.to_string()))?;
    let python = words.iter().find(|word| !word.contains('='))
        .and_then(|program| Path::new(program).file_name())
        .and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("python"));
    if python {
        let runtime = temp.path().join("runtime");
        let mut create = tokio::process::Command::new("python3");
        create.args(["-I", "-m", "venv", "--without-pip"]).arg(&runtime)
            .current_dir(temp.path()).env_remove("PYTHONPATH").env_remove("PYTHONHOME")
            .env_remove("VIRTUAL_ENV").env("PATH", std::env::join_paths(&paths).map_err(|error| SlagError::Config(error.to_string()))?)
            .kill_on_drop(true);
        checked(&mut create, "clean Python runtime").await?;
        paths.insert(0, runtime.join("bin"));
    }
    let path = std::env::join_paths(paths).map_err(|error| SlagError::Config(error.to_string()))?;
    let env = shell_words::join([
        "env", "-u", "PYTHONPATH", "-u", "PYTHONHOME", "-u", "VIRTUAL_ENV", "-u", "NODE_PATH",
        &format!("PATH={}", path.to_string_lossy()), "PYTHONDONTWRITEBYTECODE=1", "NODE_DISABLE_COMPILE_CACHE=1",
    ]);
    let command = format!("cd {} && {env} {suite}", shell_words::quote(&checkout.to_string_lossy()));
    Ok(FreshCheckout { _temp: temp, command })
}

fn clean_paths(root: &Path) -> Vec<PathBuf> {
    let active = std::env::var_os("VIRTUAL_ENV").map(PathBuf::from)
        .map(|path| path.canonicalize().unwrap_or(path));
    let inherited = std::env::var_os("PATH").map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut paths = Vec::new();
    for path in inherited {
        if !path.is_absolute() || !path.is_dir() { continue; }
        let resolved = path.canonicalize().unwrap_or(path);
        if resolved.starts_with(root) || active.as_ref().is_some_and(|active| resolved.starts_with(active))
            || resolved.components().any(|part| part.as_os_str() == ".slag" || part.as_os_str() == "logs")
            || paths.contains(&resolved) { continue; }
        paths.push(resolved);
    }
    for path in ["/usr/local/bin", "/usr/bin", "/bin"] {
        let path = PathBuf::from(path);
        if !paths.contains(&path) { paths.push(path); }
    }
    paths
}

async fn checked(command: &mut tokio::process::Command, label: &str) -> Result<(), SlagError> {
    let output = tokio::time::timeout(Duration::from_secs(30), command.output()).await
        .map_err(|_| SlagError::Config(format!("{label} timed out")))??;
    if !output.status.success() {
        return Err(SlagError::Config(format!("{label} failed: {}", String::from_utf8_lossy(&output.stderr))));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::sandbox::{git, Sandbox};

    fn repository(source: &str) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("test_clean.py"), source).unwrap();
        for args in [vec!["init", "-q"], vec!["add", "."], vec!["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "Initial commit"]] {
            assert!(git().args(args).current_dir(repo.path()).status().unwrap().success());
        }
        repo
    }

    async fn run(checkout: &FreshCheckout) -> std::process::Output {
        Sandbox::read_only().unwrap().command(&checkout.command).output().await.unwrap()
    }

    #[tokio::test]
    async fn fresh_checkout_excludes_untracked_files_and_local_runtime() {
        let repo = repository("import unittest\nclass Clean(unittest.TestCase):\n def test_clean(self):\n  import pathlib\n  self.assertFalse(pathlib.Path('untracked').exists())\n  self.assertFalse(pathlib.Path('.venv').exists())\n");
        std::fs::write(repo.path().join("untracked"), "junk").unwrap();
        std::fs::create_dir(repo.path().join(".venv")).unwrap();
        let checkout = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let output = run(&checkout).await;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Ran 1 test"));
    }

    #[tokio::test]
    async fn fresh_checkout_refuses_uncommitted_import_dependency() {
        let repo = repository("import installed_only\n");
        std::fs::write(repo.path().join("installed_only.py"), "VALUE = 1\n").unwrap();
        let checkout = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let output = run(&checkout).await;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("No module named 'installed_only'"));
    }

    #[tokio::test]
    async fn fresh_checkout_prefixed_python_uses_isolated_runtime() {
        let repo = repository("import unittest, sys\nclass Clean(unittest.TestCase):\n def test_runtime(self):\n  self.assertNotEqual(sys.prefix, sys.base_prefix)\n  self.assertFalse(__import__('site').ENABLE_USER_SITE)\n");
        let checkout = prepare(repo.path(), "PYTHONDONTWRITEBYTECODE=1 python3 -m unittest").await.unwrap();
        let output = run(&checkout).await;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Ran 1 test"));
    }

    #[tokio::test]
    async fn fresh_checkout_tests_cannot_write() {
        let repo = repository("import unittest\nclass Clean(unittest.TestCase):\n def test_write(self):\n  import pathlib\n  pathlib.Path('written').touch()\n");
        let checkout = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let output = run(&checkout).await;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("PermissionError"));
    }
}
