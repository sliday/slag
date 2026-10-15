use crate::error::SlagError;

/// Extract `CMD: <command>` from smith response text.
/// Takes the last CMD: line found (smith may output multiple).
pub fn extract_cmd(response: &str) -> Option<String> {
    response
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("CMD:"))
        .last()
        .map(|line| line.strip_prefix("CMD:").unwrap().trim().to_string())
}

/// Run a shell command and return (success, output).
pub async fn run_shell(cmd: &str) -> (bool, String) {
    run_shell_in(cmd, std::path::Path::new(".")).await
}

/// `run_shell` in `dir`. A model writes proofs and CMD lines, so they run
/// under the smith's shell sandbox rooted at `dir`. A refused write leads
/// the output, so the failed heat names it instead of showing a mystery.
pub async fn run_shell_in(cmd: &str, dir: &std::path::Path) -> (bool, String) {
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let sandbox = crate::engine::sandbox::Sandbox::for_root(&root);
    match sandbox.wrap("bash").args(["-c", cmd]).current_dir(dir).output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            let ok = output.status.success();
            match sandbox.denial_note(&combined, !ok) {
                Some(note) => (ok, format!("{note}\n{combined}")),
                None => (ok, combined),
            }
        }
        Err(e) => (false, format!("spawn error: {e}")),
    }
}

/// Verify an ingot's proof command.
/// Returns Ok(()) if proof passes, Err with reason if it fails.
pub async fn verify_proof(proof: &str, id: &str) -> Result<(), SlagError> {
    if proof.is_empty() || proof == "true" {
        return Ok(());
    }

    let (success, output) = run_shell(proof).await;
    if success {
        Ok(())
    } else {
        Err(SlagError::ProofFailed {
            id: id.to_string(),
            reason: output,
        })
    }
}

/// Serializes git mutations of the main checkout (commit, duel merge)
/// across concurrent anvils, so one anvil's `add -A`/`commit` never
/// interleaves with another's commit or a duel winner's merge.
pub static REPO_GIT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Git add + commit with forge message
pub async fn git_commit(id: &str, work: &str) {
    let _guard = REPO_GIT_LOCK.lock().await;
    let msg = format!("forge({id}): {work}");
    let _ = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .output()
        .await;
    let _ = tokio::process::Command::new("git")
        .args(["commit", "-m", &msg, "--quiet"])
        .output()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_cmd_basic() {
        let response = "Created files...\nCMD: npm test\n";
        assert_eq!(extract_cmd(response), Some("npm test".to_string()));
    }

    #[test]
    fn extract_cmd_last() {
        let response = "CMD: echo first\nmore stuff\nCMD: echo second\n";
        assert_eq!(extract_cmd(response), Some("echo second".to_string()));
    }

    #[test]
    fn extract_cmd_none() {
        let response = "No command here\njust text\n";
        assert_eq!(extract_cmd(response), None);
    }

    #[test]
    fn extract_cmd_with_spaces() {
        let response = "CMD:   test -f package.json && npm test  \n";
        assert_eq!(
            extract_cmd(response),
            Some("test -f package.json && npm test".to_string())
        );
    }

    #[tokio::test]
    async fn run_shell_success() {
        let (ok, _) = run_shell("true").await;
        assert!(ok);
    }

    #[tokio::test]
    async fn run_shell_failure() {
        let (ok, _) = run_shell("false").await;
        assert!(!ok);
    }

    /// A model-written proof that writes to `$HOME` fails, says why, and
    /// leaves nothing behind; ordinary proofs still pass under the sandbox.
    #[tokio::test]
    async fn proofs_run_in_the_sandbox() {
        use crate::engine::sandbox::{backend, Backend};
        let (_guard, _cfg) = crate::config::isolated_config_dir();
        std::env::remove_var("SLAG_SANDBOX");
        if backend() == Backend::None {
            eprintln!("skipped: no sandbox backend works here");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let probe = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
            .join("slag-sandbox-probe");
        let _ = std::fs::remove_file(&probe);
        let (ok, output) = run_shell_in("touch \"$HOME/slag-sandbox-probe\"", dir.path()).await;
        let leaked = probe.exists();
        let _ = std::fs::remove_file(&probe);
        assert!(!ok && !leaked, "the proof escaped: {output}");
        assert!(
            output.contains(&format!("blocked by slag's sandbox: {}", probe.display())),
            "{output}"
        );

        std::fs::write(dir.path().join("x"), "").unwrap();
        let (ok, output) = run_shell_in("test -f x", dir.path()).await;
        assert!(ok, "{output}");
    }

    #[tokio::test]
    async fn toolchain_proofs_pass_in_the_sandbox() {
        let (_guard, _cfg) = crate::config::isolated_config_dir();
        std::env::remove_var("SLAG_SANDBOX");
        let has = |tool: &str| {
            std::process::Command::new(tool)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let write = |dir: &std::path::Path, path: &str, body: &str| {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "rs/Cargo.toml", "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        write(root, "rs/src/lib.rs", "#[test]\nfn adds() { assert_eq!(1 + 1, 2); }\n");
        write(root, "js/package.json", r#"{"name": "p", "scripts": {"test": "node --test"}}"#);
        write(root, "js/a.test.js", "require('node:test')('adds', () => {});\n");
        write(root, "py/test_a.py", "import unittest\nclass T(unittest.TestCase):\n    def test_a(self): pass\n");
        for (tool, sub, cmd) in [
            ("cargo", "rs", "cargo test --quiet"),
            ("npm", "js", "npm test --silent"),
            ("python3", "py", "python3 -m unittest"),
        ] {
            if !has(tool) {
                eprintln!("skipped {cmd}: no {tool}");
                continue;
            }
            let (ok, output) = run_shell_in(cmd, &root.join(sub)).await;
            assert!(ok, "{cmd} failed under the sandbox:\n{output}");
        }
    }

    #[tokio::test]
    async fn verify_proof_true() {
        assert!(verify_proof("true", "i1").await.is_ok());
    }

    #[tokio::test]
    async fn verify_proof_empty() {
        assert!(verify_proof("", "i1").await.is_ok());
    }

    #[tokio::test]
    async fn verify_proof_fails() {
        let result = verify_proof("test -f /nonexistent_file_xyz", "i1").await;
        assert!(result.is_err());
    }
}
