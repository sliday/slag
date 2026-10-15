//! `slag fix` end to end against a scripted model. No network and no real
//! key: a mock chat endpoint plays the smith's turns, and the repository is
//! a throwaway git repo whose own test fails until the bug is fixed.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Plays one scripted assistant message per chat request, in order.
struct Script {
    turn: AtomicUsize,
    turns: Vec<Value>,
}

impl Respond for Script {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let n = self.turn.fetch_add(1, Ordering::SeqCst);
        let call = self.turns.get(n).cloned().unwrap_or_else(|| {
            tool("finish", json!({"summary": "out of script"}))
        });
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": format!("c{n}"), "type": "function", "function": call}],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
        }))
    }
}

fn tool(name: &str, args: Value) -> Value {
    json!({"name": name, "arguments": args.to_string()})
}

async fn model(turns: Vec<Value>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Script { turn: AtomicUsize::new(0), turns })
        .mount(&server)
        .await;
    server
}

fn sh(dir: &Path, cmd: &str) -> String {
    let out = std::process::Command::new("bash")
        .args(["-c", cmd])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{cmd}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with one bug: `value.txt` says 1, and its test wants 2.
fn buggy_repo() -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    sh(repo.path(), "git init -q -b main && git config user.email t@t && git config user.name t");
    std::fs::write(repo.path().join("value.txt"), "1\n").unwrap();
    std::fs::create_dir_all(repo.path().join("tests")).unwrap();
    std::fs::write(repo.path().join("tests/check.sh"), "grep -qx 2 value.txt\n").unwrap();
    sh(repo.path(), "git add -A && git commit -qm init");
    repo
}

/// Run `slag fix` in `repo` against `server`, with a scratch home so no
/// developer key, model pin, or session registry leaks in.
async fn slag_fix(repo: &Path, server: &MockServer, args: &[&str]) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slag"));
    for var in ["SLAG_MODEL_PLAN", "SLAG_MODEL_ALT", "SLAG_MODEL_JUDGE", "SLAG_DUEL", "SLAG_REASONING_EFFORT"] {
        cmd.env_remove(var);
    }
    cmd.env("OPENROUTER_API_KEY", "sk-or-test")
        .env("SLAG_OPENROUTER_BASE", server.uri())
        .env("SLAG_MODEL_BASE", "test/base")
        .env("SLAG_CONFIG_DIR", home.path())
        .env("SLAG_SESSIONS_DIR", home.path().join("sessions"))
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .current_dir(repo)
        .arg("fix")
        .args(args)
        .timeout(Duration::from_secs(60));
    tokio::task::spawn_blocking(move || {
        let out = cmd.output().unwrap();
        drop(home);
        out
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_verified_fix_lands_on_its_own_branch() {
    let repo = buggy_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
        tool("finish", json!({"summary": "value.txt:1 held 1; it now holds 2.", "cmd": "bash tests/check.sh"})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["value is wrong", "--test", "bash tests/check.sh"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("FIXED"), "{stdout}");
    // The test failed before the edit and passes after: that, not the
    // smith's summary, is the evidence.
    assert!(stdout.contains("gave exit 1"), "{stdout}");
    assert!(stdout.contains("`bash tests/check.sh` exit 0"), "{stdout}");

    let root = repo.path();
    assert_eq!(sh(root, "git branch --show-current").trim(), "slag/fix-value-is-wrong");
    assert_eq!(sh(root, "git log -1 --format=%s").trim(), "fix: value is wrong");
    assert_eq!(sh(root, "git show --name-only --format= HEAD").trim(), "value.txt");
    assert_eq!(sh(root, "git status --porcelain").trim(), "", "engine scratch stays out of git status");
    for plan in ["PRD.md", "BLUEPRINT.md", "PLAN.md", "PROGRESS.md"] {
        assert!(!root.join(plan).exists(), "a fix must not write {plan}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_changes_nothing_is_not_a_fix() {
    let repo = buggy_repo();
    // Claims success without touching a file.
    let server = model(vec![tool("finish", json!({"summary": "Fixed.", "cmd": "true"}))]).await;

    let out = slag_fix(
        repo.path(),
        &server,
        &["value is wrong", "--test", "bash tests/check.sh", "--tries", "1"],
    )
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("NOT FIXED"), "{stdout}");
    assert!(stdout.contains("changed no file"), "{stdout}");
    assert_eq!(sh(repo.path(), "git branch --show-current").trim(), "main", "no branch for a failure");
}

#[test]
fn fix_without_an_issue_says_how_to_give_one() {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_slag"))
        .env_remove("OPENROUTER_API_KEY")
        .env("SLAG_CONFIG_DIR", dir.path())
        .current_dir(dir.path())
        .arg("fix")
        .timeout(Duration::from_secs(30))
        .assert()
        .failure()
        .stderr(predicates::str::contains("--issue-file"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_outside_git_is_fixed_in_place() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("value.txt"), "1\n").unwrap();
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    std::fs::write(dir.path().join("tests/check.sh"), "grep -qx 2 value.txt\n").unwrap();
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
        tool("finish", json!({"summary": "value.txt now holds 2.", "cmd": "bash tests/check.sh"})),
    ])
    .await;

    let out = slag_fix(dir.path(), &server, &["value is wrong", "--test", "bash tests/check.sh"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("FIXED") && stdout.contains("value.txt +1 -1"), "{stdout}");
    assert!(stdout.contains("not a git repository"), "{stdout}");
    assert_eq!(std::fs::read_to_string(dir.path().join("value.txt")).unwrap(), "2\n");
    assert!(!dir.path().join(".git").exists(), "slag fix never turns a directory into a repo");
}
