use crate::error::SlagError;

fn broken_shell_exit(code: Option<i32>, stderr: &str) -> bool {
    matches!(code, Some(126 | 127)) || (code == Some(2) && stderr.lines().any(|line| {
        (line.starts_with("bash:") || line.contains(": line ") || line.contains(": -c:"))
            && (line.contains("syntax error") || line.contains("unexpected EOF")
                || line.contains("unexpected end of file"))
    }))
}

pub struct ShellCheck {
    pub success: bool,
    pub output: String,
    pub broken: bool,
    pub reason: String,
}

pub async fn check_shell(cmd: &str) -> ShellCheck {
    check_shell_in(cmd, std::path::Path::new(".")).await
}

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
    let result = check_shell_in(cmd, dir).await;
    (result.success, result.output)
}

async fn check_shell_in(cmd: &str, dir: &std::path::Path) -> ShellCheck {
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let sandbox = crate::engine::sandbox::Sandbox::for_root(&root);
    match sandbox.wrap("bash").args(["-c", cmd]).current_dir(dir).output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            let ok = output.status.success();
            let broken = broken_shell_exit(output.status.code(), &stderr);
            let reason = stderr.lines().find(|l| !l.trim().is_empty())
                .or_else(|| if broken { None } else { stdout.lines().find(|l| !l.trim().is_empty()) })
                .map(str::trim).map(str::to_string)
                .unwrap_or_else(|| match output.status.code() {
                    Some(code) => format!("shell exited with status {code}"),
                    None => "shell terminated by a signal".into(),
                });
            let output = match sandbox.denial_note(&combined, !ok) {
                Some(note) => (ok, format!("{note}\n{combined}")),
                None => (ok, combined),
            };
            ShellCheck { success: ok, output: output.1, broken, reason }
        }
        Err(e) => {
            let reason = format!("spawn error: {e}");
            ShellCheck { success: false, output: reason.clone(), broken: false, reason }
        }
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
    git_commit_in(std::path::Path::new("."), id, work).await;
}

/// `git_commit` in `dir`. It runs outside the sandbox, so it goes through
/// `sandbox::git_async`: a hook or fsmonitor a model planted never runs.
pub async fn git_commit_in(dir: &std::path::Path, id: &str, work: &str) {
    let _guard = REPO_GIT_LOCK.lock().await;
    let msg = format!("forge({id}): {work}");
    let _ = crate::engine::sandbox::git_async()
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .await;
    let _ = crate::engine::sandbox::git_async()
        .args(["commit", "-m", &msg, "--quiet"])
        .current_dir(dir)
        .output()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_proofs_distinguish_shell_errors_from_failed_assertions() {
        for (code, stderr, broken) in [
            (2, "bash: -c: line 1: syntax error near unexpected token `('", true),
            (2, "bash: -c: line 1: unexpected EOF while looking for matching `\"'", true),
            (126, "bash: line 1: ./check: Permission denied", true),
            (127, "bash: line 1: missing_check: command not found", true),
            (126, "", true),
            (127, "", true),
            (1, "AssertionError: expected 3", false),
            (1, "SyntaxError: invalid syntax", false),
            (2, "grep: invalid regular expression", false),
            (2, "test failed: syntax error", false),
            (0, "bash: syntax error", false),
        ] {
            assert_eq!(broken_shell_exit(Some(code), stderr), broken, "exit {code}: {stderr}");
        }
    }

    #[tokio::test]
    async fn shell_checks_keep_stderr_separate_from_progress_output() {
        let dir = tempfile::tempdir().unwrap();
        for (command, broken) in [
            ("printf 'progress\\n'; (", true),
            ("printf 'progress\\n'; slag_missing_proof_command", true),
            ("printf 'progress\\n'; printf 'assertion failed\\n' >&2; exit 1", false),
        ] {
            let result = check_shell_in(command, dir.path()).await;
            assert!(!result.success);
            assert_eq!(result.broken, broken, "{}", result.output);
            assert_ne!(result.reason, "progress");
            assert!(!result.reason.contains('\n'));
        }
        let no_stderr = check_shell_in("printf 'progress\\n'; exit 127", dir.path()).await;
        assert!(no_stderr.broken);
        assert_eq!(no_stderr.reason, "shell exited with status 127");
    }

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
        // shell-words is one of slag's own dependencies, so a machine that
        // builds slag has it in the registry cache.
        write(
            root,
            "rs/Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nshell-words = \"1\"\n",
        );
        write(
            root,
            "rs/src/lib.rs",
            "#[test]\nfn splits() { assert_eq!(shell_words::split(\"a b\").unwrap().len(), 2); }\n",
        );
        write(root, "js/package.json", r#"{"name": "p", "scripts": {"test": "node --test"}}"#);
        write(root, "js/a.test.js", "require('node:test')('adds', () => {});\n");
        write(root, "py/test_a.py", "import unittest\nclass T(unittest.TestCase):\n    def test_a(self): pass\n");
        for (tool, sub, cmd) in [
            ("cargo", "rs", "cargo test --offline --quiet"),
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

    /// A hook or fsmonitor that reached `.git` anyway (planted before the
    /// sandbox, or under Landlock) never runs in slag's own commit.
    #[tokio::test]
    async fn slag_commits_run_no_planted_hook_or_fsmonitor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let marker = |name: &str| root.join(name);
        let sh = |cmd: &str| {
            let ok = std::process::Command::new("bash")
                .args(["-c", cmd])
                .current_dir(root)
                .status()
                .unwrap()
                .success();
            assert!(ok, "{cmd}");
        };
        sh("git init -q -b main && git config user.email t@t && git config user.name t");
        sh(&format!(
            "printf '#!/bin/sh\\ntouch {}\\n' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit",
            marker("hook-ran").display()
        ));
        sh(&format!("git config core.fsmonitor 'touch {}'", marker("fsmonitor-ran").display()));
        std::fs::write(root.join("a.txt"), "a\n").unwrap();

        git_commit_in(root, "i1", "plant check").await;
        let status = crate::engine::sandbox::git()
            .args(["status", "--short"])
            .current_dir(root)
            .output()
            .unwrap();

        let log = crate::engine::sandbox::git()
            .args(["log", "--oneline"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("forge(i1)"), "commit made");
        assert!(status.status.success());
        assert!(!marker("hook-ran").exists(), "slag's commit ran the planted hook");
        assert!(!marker("fsmonitor-ran").exists(), "slag's git ran the planted fsmonitor");
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
