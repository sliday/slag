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

pub(super) enum InstallCheck {
    Ready(FreshCheckout),
    Skipped(String),
}

#[derive(Clone, Copy)]
enum InstallMethod { Venv, Virtualenv, Target }

pub(super) async fn prepare_install(root: &Path) -> Result<InstallCheck, SlagError> {
    prepare_install_with(root, &[InstallMethod::Venv, InstallMethod::Virtualenv, InstallMethod::Target]).await
}

async fn prepare_install_with(root: &Path, methods: &[InstallMethod]) -> Result<InstallCheck, SlagError> {
    let mut prepared = prepare(root, "true").await?;
    let root = prepared._temp.path();
    let checkout = root.join("checkout");
    let runtime = root.join("installed-runtime");
    let temp = root.join("scratch");
    std::fs::create_dir(&temp)?;
    let sandbox = crate::engine::sandbox::Sandbox::for_scratch(root)?;
    let mut selected = None;
    for method in methods {
        let (module, args) = match method {
            InstallMethod::Venv => ("venv", vec![runtime.to_string_lossy().into_owned()]),
            InstallMethod::Virtualenv => ("virtualenv", virtualenv_args(root, &runtime)),
            InstallMethod::Target => ("pip", vec!["--version".to_string()]),
        };
        if runtime.exists() { std::fs::remove_dir_all(&runtime)?; }
        let mut create = sandbox.wrap("python3");
        create.args(["-I", "-m", module]).args(args).current_dir(root)
            .env("VIRTUALENV_NO_PERIODIC_UPDATE", "1").env_remove("PYTHONPATH").env_remove("PYTHONHOME").env_remove("VIRTUAL_ENV")
            .env("TMPDIR", &temp).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(30), create.output()).await
            .map_err(|_| SlagError::Config("isolated installation runtime timed out".into()))??;
        if output.status.success() {
            selected = Some(*method);
            break;
        }
        let error = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if !missing_install_tool(&error) {
            return Err(SlagError::Config(format!("isolated installation runtime failed: {error}")));
        }
    }
    let Some(method) = selected else {
        return Ok(InstallCheck::Skipped("Python venv/ensurepip, virtualenv, and pip are unavailable".into()));
    };
    let target = root.join("installed-target");
    let targeted = matches!(method, InstallMethod::Target);
    let mut python = if targeted { PathBuf::from("python3") } else { runtime.join("bin/python") };
    if targeted {
        let mut create = sandbox.wrap("python3");
        create.args(["-I", "-m", "venv", "--without-pip"]).arg(&runtime).current_dir(root)
            .env_remove("PYTHONPATH").env_remove("PYTHONHOME").env_remove("VIRTUAL_ENV")
            .env("TMPDIR", &temp).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(30), create.output()).await
            .map_err(|_| SlagError::Config("isolated target runtime timed out".into()))??;
        if output.status.success() { python = runtime.join("bin/python"); }
        else {
            let error = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            if !missing_install_tool(&error) { return Err(SlagError::Config(format!("isolated target runtime failed: {error}"))); }
        }
    }
    if targeted && python != Path::new("python3") {
        let mut probe = sandbox.wrap("python3");
        probe.args(["-I", "-m", "pip", "--python"]).arg(&python).arg("--version")
            .current_dir(root).env_remove("PYTHONPATH").env_remove("PYTHONHOME").env_remove("VIRTUAL_ENV")
            .env("TMPDIR", &temp).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(30), probe.output()).await
            .map_err(|_| SlagError::Config("isolated pip probe timed out".into()))??;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("no such option: --python") {
                return Ok(InstallCheck::Skipped("host pip support for an isolated Python runtime is unavailable (--python)".into()));
            }
            return Err(SlagError::Config(format!("isolated pip probe failed: {error}")));
        }
    }
    let launcher = if targeted { "python3" } else { python.to_str().ok_or_else(|| SlagError::Config("non-UTF-8 installation path".into()))? };
    let mut install = sandbox.wrap(launcher);
    install.args(["-I", "-m", "pip"]);
    if targeted && python != Path::new("python3") { install.arg("--python").arg(&python); }
    install.args(["--isolated", "--timeout", "10", "--retries", "0", "install", "--disable-pip-version-check", "--no-input", "--no-deps", "--no-cache-dir", "--progress-bar", "off"]);
    if targeted { install.arg("--target").arg(&target); }
    install.arg(".").current_dir(&checkout).env_remove("PYTHONPATH").env_remove("PYTHONHOME").env_remove("VIRTUAL_ENV")
        .env("TMPDIR", &temp).kill_on_drop(true);
    checked(&mut install, "package installation in a temporary clone").await?;
    let probe = root.join("installed-console-check.py");
    std::fs::write(&probe, INSTALL_CHECK)?;
    let mut command = vec![python.to_string_lossy().into_owned(), if targeted { "-s".into() } else { "-I".into() },
        probe.to_string_lossy().into_owned(), checkout.to_string_lossy().into_owned()];
    if targeted {
        command.push(target.to_string_lossy().into_owned());
        command.splice(0..0, ["env".into(), "-u".into(), "PYTHONHOME".into(), "-u".into(), "VIRTUAL_ENV".into(),
            format!("PYTHONPATH={}", target.to_string_lossy()), "PYTHONDONTWRITEBYTECODE=1".into()]);
    }
    prepared.command = shell_words::join(command);
    Ok(InstallCheck::Ready(prepared))
}

fn virtualenv_args(root: &Path, runtime: &Path) -> Vec<String> {
    vec!["--app-data".into(), root.join("virtualenv-data").to_string_lossy().into_owned(),
        runtime.to_string_lossy().into_owned()]
}

fn missing_install_tool(error: &str) -> bool {
    let error = error.split_whitespace().collect::<Vec<_>>().join(" ");
    error.contains("No module named venv") || error.contains("No module named virtualenv")
        || error.contains("No module named pip") || error.contains("ensurepip is not available")
        || error.contains("No module named ensurepip")
}

const INSTALL_CHECK: &str = r#"import importlib.metadata, json, os, pathlib, subprocess, sys
source = pathlib.Path(sys.argv[1]).resolve().as_uri()
entries = []
target = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else None
environment = {key: value for key, value in os.environ.items() if key not in ('PYTHONPATH', 'PYTHONHOME', 'VIRTUAL_ENV')}
environment['PYTHONDONTWRITEBYTECODE'] = '1'
if target:
    environment['PYTHONPATH'] = str(target)
    environment['PYTHONNOUSERSITE'] = '1'
for distribution in importlib.metadata.distributions(**({'path': [str(target)]} if target else {})):
    direct = distribution.read_text('direct_url.json')
    if direct and json.loads(direct).get('url', '').rstrip('/') == source.rstrip('/'):
        entries.extend(entry for entry in distribution.entry_points if entry.group == 'console_scripts')
assert entries, 'the installed package has no console-script entry point'
for entry in entries:
    assert pathlib.Path(entry.name).name == entry.name and entry.name not in ('.', '..'), entry.name
    command = (target / 'bin' if target else pathlib.Path(sys.executable).parent) / entry.name
    result = subprocess.run([str(command), '--help'], cwd=pathlib.Path(__file__).parent, env=environment, text=True, capture_output=True, timeout=10)
    assert result.returncode == 0, (entry.name, result.returncode, result.stdout, result.stderr)
    assert result.stdout.strip() or result.stderr.strip(), 'console --help produced no output'
    print('Installed console script:', entry.name)
    print(result.stdout, end='')
"#;

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

    fn enforced(sandbox: std::io::Result<Sandbox>) -> Option<Sandbox> {
        match sandbox {
            Ok(sandbox) => Some(sandbox),
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::Unsupported, "{error}");
                assert!(error.to_string().contains("Landlock cannot block"), "{error}");
                None
            }
        }
    }

    async fn run(checkout: &FreshCheckout) -> Option<std::process::Output> {
        let sandbox = enforced(Sandbox::read_only())?;
        Some(sandbox.command(&checkout.command).output().await.unwrap())
    }

    #[tokio::test]
    async fn fresh_checkout_excludes_untracked_files_and_local_runtime() {
        let repo = repository("import unittest\nclass Clean(unittest.TestCase):\n def test_clean(self):\n  import pathlib\n  self.assertFalse(pathlib.Path('untracked').exists())\n  self.assertFalse(pathlib.Path('.venv').exists())\n");
        std::fs::write(repo.path().join("untracked"), "junk").unwrap();
        std::fs::create_dir(repo.path().join(".venv")).unwrap();
        let checkout = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let Some(output) = run(&checkout).await else { return };
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Ran 1 test"));
    }

    #[tokio::test]
    async fn fresh_checkout_refuses_uncommitted_import_dependency() {
        let repo = repository("import installed_only\n");
        std::fs::write(repo.path().join("installed_only.py"), "VALUE = 1\n").unwrap();
        let checkout = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let Some(output) = run(&checkout).await else { return };
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("No module named 'installed_only'"));
    }

    #[tokio::test]
    async fn fresh_checkout_prefixed_python_uses_isolated_runtime() {
        let repo = repository("import unittest, sys\nclass Clean(unittest.TestCase):\n def test_runtime(self):\n  self.assertNotEqual(sys.prefix, sys.base_prefix)\n  self.assertFalse(__import__('site').ENABLE_USER_SITE)\n");
        let checkout = prepare(repo.path(), "PYTHONDONTWRITEBYTECODE=1 python3 -m unittest").await.unwrap();
        let Some(output) = run(&checkout).await else { return };
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Ran 1 test"));
    }

    #[tokio::test]
    async fn isolated_install_checks_console_without_workspace_residue() {
        assert_install_methods(&[InstallMethod::Venv, InstallMethod::Virtualenv, InstallMethod::Target]).await;
    }

    #[tokio::test]
    async fn isolated_install_falls_back_to_target_without_workspace_residue() {
        assert_install_methods(&[InstallMethod::Target]).await;
    }

    async fn assert_install_methods(methods: &[InstallMethod]) {
        let repo = repository("import unittest\nclass Clean(unittest.TestCase):\n def test_source(self):\n  import example_cli\n  self.assertTrue(callable(example_cli.main))\n");
        std::fs::write(repo.path().join("example_cli.py"), "def main():\n print('usage: example-cli [--help]')\n").unwrap();
        std::fs::write(repo.path().join("pyproject.toml"), "[build-system]\nrequires = []\nbuild-backend = 'backend'\nbackend-path = ['.']\n").unwrap();
        std::fs::write(repo.path().join("backend.py"), r#"import pathlib, zipfile
def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    pathlib.Path('example_cli.egg-info').mkdir(exist_ok=True)
    pathlib.Path('.venv').mkdir(exist_ok=True)
    filename = 'example_cli-1.0-py3-none-any.whl'
    prefix = 'example_cli-1.0.dist-info/'
    with zipfile.ZipFile(pathlib.Path(wheel_directory) / filename, 'w') as wheel:
        wheel.write('example_cli.py')
        wheel.writestr(prefix + 'METADATA', 'Metadata-Version: 2.1\nName: example-cli\nVersion: 1.0\n')
        wheel.writestr(prefix + 'WHEEL', 'Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n')
        wheel.writestr(prefix + 'entry_points.txt', '[console_scripts]\nexample-cli = example_cli:main\n')
        wheel.writestr(prefix + 'RECORD', '')
    return filename
"#).unwrap();
        let backend = repo.path().join("backend.py");
        let source = std::fs::read_to_string(&backend).unwrap();
        let original = serde_json::to_string(&repo.path().join("install-escaped").to_string_lossy()).unwrap();
        let guarded = source.replace("    filename =", &format!(
            "    try:\n        pathlib.Path({original}).write_text('escape')\n    except OSError as error:\n        if error.errno not in (1, 13, 30):\n            raise\n    else:\n        raise AssertionError('installation wrote into the original repository')\n    filename ="));
        std::fs::write(backend, guarded).unwrap();
        assert!(git().args(["add", "."]).current_dir(repo.path()).status().unwrap().success());
        assert!(git().args(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "Add package"])
            .current_dir(repo.path()).status().unwrap().success());
        let fresh = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let Some(suite) = run(&fresh).await else { return };
        assert!(suite.status.success(), "{}", String::from_utf8_lossy(&suite.stderr));
        let installed = match prepare_install_with(repo.path(), methods).await.unwrap() {
            InstallCheck::Ready(checkout) => checkout,
            InstallCheck::Skipped(reason) => {
                assert!(reason.contains("unavailable"));
                assert!(!repo.path().join(".venv").exists());
                assert!(!repo.path().join("example_cli.egg-info").exists());
                assert!(!repo.path().join("install-escaped").exists());
                return;
            }
        };
        let installed_root = installed._temp.path().to_path_buf();
        let Some(result) = run(&installed).await else { return };
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        assert!(String::from_utf8_lossy(&result.stdout).contains("Installed console script: example-cli"));
        assert!(!repo.path().join(".venv").exists());
        assert!(!repo.path().join("example_cli.egg-info").exists());
        assert!(!repo.path().join("install-escaped").exists());
        assert!(!fresh._temp.path().join("checkout/.venv").exists());
        drop(installed);
        assert!(!installed_root.exists());
    }

    #[tokio::test]
    async fn isolated_install_missing_tools_skips_without_a_proof_command() {
        let repo = repository("import unittest\n");
        let Some(_sandbox) = enforced(Sandbox::for_scratch(repo.path())) else { return };
        match prepare_install_with(repo.path(), &[]).await.unwrap() {
            InstallCheck::Skipped(reason) => assert!(reason.contains("unavailable")),
            InstallCheck::Ready(_) => panic!("missing installation tools produced a proof command"),
        }
    }

    #[tokio::test]
    async fn isolated_install_package_failures_remain_errors() {
        let repo = repository("import unittest\n");
        let Some(_sandbox) = enforced(Sandbox::for_scratch(repo.path())) else { return };
        std::fs::write(repo.path().join("pyproject.toml"), "[invalid TOML").unwrap();
        assert!(git().args(["add", "."]).current_dir(repo.path()).status().unwrap().success());
        assert!(git().args(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "Invalid package"])
            .current_dir(repo.path()).status().unwrap().success());
        match prepare_install(repo.path()).await {
            Err(error) => assert!(error.to_string().contains("package installation in a temporary clone failed"), "{error}"),
            Ok(InstallCheck::Skipped(reason)) => assert!(reason.contains("unavailable")),
            Ok(InstallCheck::Ready(_)) => panic!("invalid package passed installation"),
        }
    }

    #[test]
    fn virtualenv_keeps_app_data_in_private_installation_directory() {
        let root = Path::new("/private-install");
        let runtime = root.join("runtime");
        assert_eq!(virtualenv_args(root, &runtime), ["--app-data", "/private-install/virtualenv-data", "/private-install/runtime"]);
    }

    #[test]
    fn install_tool_detection_does_not_hide_package_failures() {
        assert!(missing_install_tool("The virtual environment was not created because ensurepip is not available"));
        assert!(missing_install_tool("/usr/bin/python3: No module named pip"));
        assert!(missing_install_tool("The virtual environment was not created successfully because ensurepip is not\navailable.  On Debian/Ubuntu systems, you need to install the python3-venv\npackage using the following command."));
        assert!(!missing_install_tool("ERROR: Failed building wheel for invalid-package"));
        assert!(!missing_install_tool("Permission denied"));
    }

    #[tokio::test]
    async fn fresh_checkout_suite_can_use_private_tempfile() {
        let repo = repository("import unittest, tempfile, pathlib\nclass Clean(unittest.TestCase):\n def test_temp(self):\n  with tempfile.TemporaryDirectory() as temp:\n   pathlib.Path(temp, 'log').write_text('new line')\n");
        let checkout = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let Some(output) = run(&checkout).await else { return };
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    #[tokio::test]
    async fn fresh_checkout_tests_cannot_write() {
        let repo = repository("import unittest\nclass Clean(unittest.TestCase):\n def test_write(self):\n  import pathlib\n  pathlib.Path('written').touch()\n");
        let checkout = prepare(repo.path(), "python3 -m unittest").await.unwrap();
        let Some(output) = run(&checkout).await else { return };
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "stdout: {}\nstderr: {stderr}", String::from_utf8_lossy(&output.stdout));
        assert!(stderr.contains("Ran 1 test"), "{stderr}");
        assert!(stderr.contains("PermissionError") || stderr.contains("Read-only file system"), "{stderr}");
        assert!(!checkout._temp.path().join("checkout/written").exists());
    }
}
