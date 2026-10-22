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
        // A plain-text turn: the reviewer's reply, which carries no tools.
        if let Some(text) = call.get("text").and_then(Value::as_str) {
            return ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {"role": "assistant", "content": text},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
            }));
        }
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

/// The suite: every `tests/*.sh` must pass.
const SUITE: &str = "for t in tests/*.sh; do bash $t || exit 1; done";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reviewer_gap_gets_one_revision_that_lands_with_the_fix() {
    let repo = buggy_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/value_test.sh", "content": "grep -qx 2 value.txt\n"})),
        tool("finish", json!({"summary": "value.txt:1 now holds 2.", "cmd": SUITE})),
        json!({"text": "VERDICT: fail\nGAP: name the test after the reported case: tests/test_value_is_two.sh"}),
        tool("write_file", json!({"path": "tests/test_value_is_two.sh", "content": "grep -qx 2 value.txt\n"})),
        tool("finish", json!({"summary": "Added the named test.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["value is wrong", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("revised once after review"), "{stdout}");
    let files = sh(repo.path(), "git show --name-only --format= HEAD");
    assert!(files.contains("tests/test_value_is_two.sh") && files.contains("value.txt"), "{files}");
}

/// The pycparser shape: the first fix and its test cover the reported input
/// and pass their own proof. The review names one variant the test never
/// tries; that gap gets the one revision, quoted in its brief, and no more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bug_class_gap_gets_one_revision_that_names_the_variant() {
    let repo = buggy_repo();
    let gap = "the test tries only the reported read; also read value.txt through cat in tests/value_test.sh";
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/value_test.sh", "content": "grep -qx 2 value.txt\n"})),
        tool("finish", json!({"summary": "value.txt:1 now holds 2.", "cmd": SUITE})),
        json!({"text": format!("VERDICT: fail\nGAP: {gap}")}),
        tool("read_file", json!({"path": "tests/value_test.sh"})),
        tool("write_file", json!({
            "path": "tests/value_test.sh",
            "content": "grep -qx 2 value.txt && test \"$(cat value.txt)\" = 2\n",
        })),
        tool("finish", json!({"summary": "The test reads the value both ways.", "cmd": SUITE})),
        // Never used: one revision, no second review.
        json!({"text": "VERDICT: fail\nGAP: also read value.txt through head in tests/value_test.sh"}),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["value is wrong", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("revised once after review; the revision closed the gap"), "{stdout}");
    let bodies: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    // A session's first request carries its brief; later turns repeat it.
    let briefs: Vec<String> = bodies
        .iter()
        .filter(|b| b["messages"].as_array().is_some_and(|m| m.iter().all(|m| m["role"] != "assistant")))
        .map(Value::to_string)
        .collect();
    assert!(briefs[0].contains("one example of a bug class"), "{}", briefs[0]);
    let reviews: Vec<&String> = briefs.iter().filter(|b| b.contains("ROLE: reviewer of a bug fix")).collect();
    assert_eq!(reviews.len(), 1, "one review");
    assert!(reviews[0].contains("BUG CLASS"), "{}", reviews[0]);
    let revisions: Vec<&String> = briefs.iter().filter(|b| b.contains("REVIEW: ONE GAP LEFT")).collect();
    assert_eq!(revisions.len(), 1, "exactly one revision brief");
    assert!(revisions[0].contains(gap), "{}", revisions[0]);
    assert_eq!(sh(repo.path(), "git show HEAD:tests/value_test.sh").trim(), "grep -qx 2 value.txt && test \"$(cat value.txt)\" = 2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revision_that_breaks_the_fix_is_rolled_back() {
    let repo = buggy_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/value_test.sh", "content": "grep -qx 2 value.txt\n"})),
        tool("finish", json!({"summary": "value.txt:1 now holds 2.", "cmd": SUITE})),
        json!({"text": "VERDICT: fail\nGAP: also cover the value 3"}),
        // The revision breaks the fix the tests had passed.
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "2", "new_string": "3"})),
        tool("write_file", json!({"path": "tests/extra.sh", "content": "true\n"})),
        tool("finish", json!({"summary": "Changed it.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["value is wrong", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("kept the first fix"), "{stdout}");
    assert_eq!(std::fs::read_to_string(repo.path().join("value.txt")).unwrap(), "2\n");
    assert!(!repo.path().join("tests/extra.sh").exists(), "the revision's new file is gone");
    let files = sh(repo.path(), "git show --name-only --format= HEAD");
    assert_eq!(files.trim(), "tests/value_test.sh\nvalue.txt", "{files}");
}

/// Two values; the issue is about `b`, and an existing test pins `a`.
fn two_value_repo() -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    sh(repo.path(), "git init -q -b main && git config user.email t@t && git config user.name t");
    std::fs::write(repo.path().join("a.txt"), "1\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "1\n").unwrap();
    std::fs::create_dir_all(repo.path().join("tests")).unwrap();
    std::fs::write(repo.path().join("tests/a.sh"), "grep -qx 1 a.txt\n").unwrap();
    sh(repo.path(), "git add -A && git commit -qm init");
    repo
}

/// The js-notify-preview shape: the fix breaks an existing expectation,
/// and the session rewrites the expectation to match. The original test,
/// run against the fix, fails, and nothing said the issue asked for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rewriting_an_existing_expectation_to_fit_the_fix_is_not_a_fix() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "a.txt"})),
        tool("edit_file", json!({"path": "a.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "tests/a.sh"})),
        tool("edit_file", json!({"path": "tests/a.sh", "old_string": "-qx 1", "new_string": "-qx 2"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 2 b.txt\n"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE, "--tries", "1"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("NOT FIXED") && stdout.contains("breaks existing tests"), "{stdout}");
    assert_eq!(sh(repo.path(), "git branch --show-current").trim(), "main");
}

/// The py-passcheck shape: an existing test rewritten though the original
/// still passes. The attempt goes back to restore it; the retry that does
/// lands, and the commit leaves the old test alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unneeded_rewrite_of_an_old_test_goes_back_and_the_retry_lands() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "tests/a.sh"})),
        tool("write_file", json!({"path": "tests/a.sh", "content": "test \"$(cat a.txt)\" = 1\n"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 2 b.txt\n"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": SUITE})),
        // The retry restores the old test exactly.
        tool("read_file", json!({"path": "tests/a.sh"})),
        tool("write_file", json!({"path": "tests/a.sh", "content": "grep -qx 1 a.txt\n"})),
        tool("finish", json!({"summary": "Restored tests/a.sh.", "cmd": SUITE})),
        json!({"text": "VERDICT: pass\nGAP:"}),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE, "--tries", "2"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("edited existing tests that pass unchanged: tests/a.sh"), "{stdout}");
    let files = sh(repo.path(), "git show --name-only --format= HEAD");
    assert_eq!(files.trim(), "b.txt\ntests/b.sh", "{files}");
}

/// The rust-hawkconf shape: an existing test deleted outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_an_existing_test_is_not_a_fix() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("bash", json!({"command": "rm tests/a.sh"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 2 b.txt\n"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE, "--tries", "1"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("tests/a.sh"), "the deleted test is named: {stdout}");
}

/// When the issue asks for the very behavior an old test pins, the old
/// test may change, said in plain sight; the bug is still shown by a new
/// test, with the old one put back as it was for the reproduction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_change_the_issue_asks_for_lands_in_plain_sight() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "a.txt"})),
        tool("edit_file", json!({"path": "a.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "tests/a.sh"})),
        tool("edit_file", json!({"path": "tests/a.sh", "old_string": "-qx 1", "new_string": "-qx 2"})),
        tool("write_file", json!({"path": "tests/a_is_two.sh", "content": "grep -qx 2 a.txt\n"})),
        tool("finish", json!({
            "summary": "a.txt now holds 2.\nchanged existing tests: tests/a.sh: the issue asks a to be 2",
            "cmd": SUITE,
        })),
        // The usual review, then the one a declared change must pass.
        json!({"text": "VERDICT: pass\nGAP:"}),
        json!({"text": format!("VERDICT: pass\nGAP:\nTEST_EDITS: {}", json!([{
            "path": "tests/a.sh",
            "removed_behavior_wrong": "yes",
            "added_behavior_requested": "yes",
            "preserves_checks": "yes",
            "issue_quote": "a.txt must hold 2, not 1",
            "removed_quote": "grep -qx 1 a.txt",
            "added_quote": "grep -qx 2 a.txt",
        }]))}),
    ])
    .await;

    let issue = "a.txt must hold 2, not 1; tests/a.sh pins the old value and must change";
    let out = slag_fix(repo.path(), &server, &[issue, "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("changed existing tests: tests/a.sh"), "{stdout}");
    assert!(stdout.contains("without the fix"), "a new test shows the bug: {stdout}");
    let body = sh(repo.path(), "git log -1 --format=%B");
    assert!(body.contains("changed existing tests: tests/a.sh"), "{body}");
    // The agreement review has no revision behind it, so it asks nothing
    // about the bug class.
    let reviews: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .filter(|b| b.contains("ROLE: reviewer of a bug fix"))
        .collect();
    assert_eq!(reviews.len(), 2);
    assert!(reviews[0].contains("BUG CLASS") && !reviews[1].contains("BUG CLASS"));
}

async fn correct_buggy_expectation(answer: &str) -> (TempDir, MockServer, std::process::Output) {
    let repo = two_value_repo();
    let review = json!({
        "path": "tests/a.sh",
        "removed_behavior_wrong": answer,
        "added_behavior_requested": "yes",
        "preserves_checks": "yes",
        "issue_quote": "a.txt contains 1, which is wrong. It must contain 2.",
        "removed_quote": "grep -qx 1 a.txt",
        "added_quote": "grep -qx 2 a.txt",
    });
    let server = model(vec![
        tool("read_file", json!({"path": "a.txt"})),
        tool("edit_file", json!({"path": "a.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "tests/a.sh"})),
        tool("edit_file", json!({"path": "tests/a.sh", "old_string": "-qx 1", "new_string": "-qx 2"})),
        tool("write_file", json!({"path": "tests/a_is_two.sh", "content": "grep -qx 2 a.txt\n"})),
        tool("finish", json!({
            "summary": "Corrected a.txt.\nchanged existing tests: tests/a.sh: it asserts the reported wrong value",
            "cmd": SUITE,
        })),
        json!({"text": "VERDICT: pass\nGAP:"}),
        json!({"text": format!("VERDICT: pass\nGAP:\nTEST_EDITS: {}", json!([review]))}),
    ])
    .await;
    let issue = "a.txt contains 1, which is wrong. It must contain 2.";
    let out = slag_fix(repo.path(), &server, &[issue, "--test", SUITE, "--tries", "1"]).await;
    (repo, server, out)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_expectation_of_the_reported_bug_can_change_with_quoted_review() {
    let (repo, server, out) = correct_buggy_expectation("yes").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("changed existing tests: tests/a.sh"), "{stdout}");
    assert!(stdout.contains("without the fix"), "{stdout}");
    assert_eq!(sh(repo.path(), "git show HEAD:tests/a.sh").trim(), "grep -qx 2 a.txt");
    assert!(sh(repo.path(), "git log -1 --format=%B").contains("changed existing tests: tests/a.sh"));
    let requests = server.received_requests().await.unwrap();
    let review = requests.iter().filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .filter_map(|v| v["messages"][0]["content"].as_str().map(str::to_owned))
        .find(|p| p.contains("TEST_EDITS:")).expect("a structured review question");
    for text in ["a.txt contains 1, which is wrong", "grep -qx 1 a.txt", "grep -qx 2 a.txt", "preserves_checks"] {
        assert!(review.contains(text), "{text} missing: {review}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_review_no_on_the_reported_bug_keeps_the_existing_test_guard() {
    let (repo, _server, out) = correct_buggy_expectation("no").await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("NOT FIXED") && stdout.contains("without the review agreeing"), "{stdout}");
    assert_eq!(sh(repo.path(), "git branch --show-current").trim(), "main");
}

/// The sh-backup-rotate shape: the session read the reported behavior as
/// the intended one and wrote a test that pins the bug, changing no code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_to_tests_alone_is_not_a_fix() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 1 b.txt\n"})),
        tool("finish", json!({"summary": "b.txt is correct as 1.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE, "--tries", "1"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("only tests changed"), "{stdout}");
}

/// A reviewer can be wrong. Here it tells the session to pin a different
/// value, and the revision changes the source to match: the first fix's
/// own test, run against the revised source, fails, so the first fix is
/// what lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revision_that_follows_a_wrong_review_is_rolled_back() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 2 b.txt\n"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": SUITE})),
        json!({"text": "VERDICT: fail\nGAP: b should hold 3; change b.txt and tests/b.sh to 3"}),
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "2", "new_string": "3"})),
        tool("read_file", json!({"path": "tests/b.sh"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 3 b.txt\n"})),
        tool("finish", json!({"summary": "b.txt now holds 3.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("broke what the first fix's tests pin"), "{stdout}");
    assert_eq!(std::fs::read_to_string(repo.path().join("b.txt")).unwrap(), "2\n");
    assert_eq!(sh(repo.path(), "git show HEAD:tests/b.sh").trim(), "grep -qx 2 b.txt");
}

/// A source change that no test can tell apart from the old code: the new
/// test passes with it reverted, and the one revision does not change
/// that. Nothing shows the bug is gone, so the verdict says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_no_test_can_see_is_not_called_fixed() {
    let repo = two_value_repo();
    let server = model(vec![
        // A no-op edit, and a test that holds either way.
        tool("read_file", json!({"path": "b.txt"})),
        tool("write_file", json!({"path": "b.txt", "content": "1\n\n"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -q 1 b.txt\n"})),
        tool("finish", json!({"summary": "Fixed b.", "cmd": SUITE})),
        // The revision changes nothing that matters either.
        tool("read_file", json!({"path": "tests/b.sh"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -q 1 b.txt # b\n"})),
        tool("finish", json!({"summary": "Sharpened the test.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("no test shows the change fixes anything"), "{stdout}");
}

/// The pyparsing shape: the new test never finishes against the original
/// code. A timeout without the fix IS the reproduction, sized from the
/// passing run, so the proof costs seconds, not the full TEST_TIMEOUT.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_check_that_hangs_without_the_fix_is_the_reproduction() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
    std::fs::write(root.join("value.txt"), "1\n").unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("tests/check.sh"),
        "for t in tests/test_*.sh; do [ -e \"$t\" ] || continue; bash \"$t\" || exit 1; done\n",
    )
    .unwrap();
    sh(root, "git add -A && git commit -qm init");
    let check = "bash tests/check.sh";
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "0"})),
        tool(
            "write_file",
            json!({"path": "tests/test_hang.sh", "content": "while grep -qx 1 value.txt; do sleep 0.2; done\n"}),
        ),
        tool("finish", json!({"summary": "value.txt now holds 0; the read ends.", "cmd": check})),
        json!({"text": "VERDICT: pass\nGAP:"}),
    ])
    .await;

    let started = std::time::Instant::now();
    let out = slag_fix(root, &server, &["reads hang while value is 1", "--test", check]).await;
    let secs = started.elapsed().as_secs();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("FIXED"), "{stdout}");
    assert!(stdout.contains("timed out after 30s"), "the without-fix check got its own timeout: {stdout}");
    assert!(stdout.contains("the code hangs"), "the hang is the reproduction: {stdout}");
    assert!(secs < 50, "the proof must not wait out TEST_TIMEOUT: {secs}s");
    let files = sh(root, "git show --name-only --format= HEAD");
    assert!(files.contains("tests/test_hang.sh") && files.contains("value.txt"), "{files}");
}

/// The first test passes with and without the fix: one revision request
/// through the review-gap path, and the revision's sharper test, failing
/// without the fix, is the reproduction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_that_passes_without_the_fix_gets_one_revision() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -q . b.txt\n"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": SUITE})),
        // The revision makes the test fail on the original code.
        tool("read_file", json!({"path": "tests/b.sh"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 2 b.txt\n"})),
        tool("finish", json!({"summary": "The test now wants 2 exactly.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("FIXED"), "{stdout}");
    assert!(stdout.contains("revised once after review"), "{stdout}");
    assert!(stdout.contains("the new test catches the bug"), "{stdout}");
    // The revision was briefed with the measured gap.
    let revision = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .find(|body| body.contains("REVIEW: ONE GAP LEFT"))
        .expect("a revision was briefed");
    assert!(revision.contains("passes with the source change reverted"), "{revision}");
}

/// The one revision spent, the test still passes with and without the
/// change: the verdict stays honest instead of calling it fixed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revision_that_still_proves_nothing_is_not_called_fixed() {
    let repo = two_value_repo();
    let server = model(vec![
        // No test at all in the first attempt.
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": SUITE})),
        // The revision adds a test, but one that holds either way.
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -q . b.txt\n"})),
        tool("finish", json!({"summary": "Added a test.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("no test shows the change fixes anything"), "{stdout}");
}

/// The user's own files never ride along: an untracked note, a tracked
/// file with uncommitted edits, and an output file their shell redirects
/// into that grows during the run. All three stay out of the commit and
/// keep exactly what they hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_users_own_uncommitted_files_never_reach_the_commit() {
    let repo = buggy_repo();
    let root = repo.path();
    std::fs::write(root.join("README.md"), "# app\n").unwrap();
    sh(root, "git add README.md && git commit -qm readme");
    std::fs::write(root.join("README.md"), "# app\n\nmy unfinished notes\n").unwrap();
    std::fs::write(root.join("notes.txt"), "api key: do-not-commit\n").unwrap();
    std::fs::write(root.join("out.txt"), "").unwrap();
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/value_test.sh", "content": "grep -qx 2 value.txt\n"})),
        tool("bash", json!({"command": "echo progress >> out.txt"})),
        tool("finish", json!({"summary": "value.txt:1 now holds 2.", "cmd": SUITE})),
        json!({"text": "VERDICT: pass\nGAP:"}),
    ])
    .await;

    let out = slag_fix(root, &server, &["value is wrong", "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    let files = sh(root, "git show --name-only --format= HEAD");
    assert_eq!(files.trim(), "tests/value_test.sh\nvalue.txt", "{files}");
    assert_eq!(std::fs::read_to_string(root.join("README.md")).unwrap(), "# app\n\nmy unfinished notes\n");
    assert_eq!(std::fs::read_to_string(root.join("notes.txt")).unwrap(), "api key: do-not-commit\n");
    assert_eq!(std::fs::read_to_string(root.join("out.txt")).unwrap(), "progress\n");
    let status = sh(root, "git status --porcelain");
    for path in ["README.md", "notes.txt", "out.txt"] {
        assert!(status.contains(path), "{path} stays uncommitted: {status}");
    }
    assert!(stdout.contains("left untouched: 3 files you had before"), "{stdout}");
}

/// A declared change to an existing test whose changed lines share nothing
/// with the issue: the issue does not ask for it, so it is not a fix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_change_the_issue_never_mentions_is_not_a_fix() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "a.txt"})),
        tool("edit_file", json!({"path": "a.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "tests/a.sh"})),
        tool("edit_file", json!({"path": "tests/a.sh", "old_string": "-qx 1", "new_string": "-qx 2"})),
        tool("write_file", json!({"path": "tests/b.sh", "content": "grep -qx 2 a.txt\n"})),
        tool("finish", json!({
            "summary": "Done.\nchanged existing tests: tests/a.sh: the new value is right",
            "cmd": SUITE,
        })),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE, "--tries", "1"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("the issue never mentions"), "{stdout}");
    assert_eq!(std::fs::read_to_string(repo.path().join("tests/a.sh")).unwrap(), "grep -qx 1 a.txt\n");
}

/// A declared change the issue does name still needs a review that says
/// pass in so many words. A reply with no verdict is no agreement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_change_without_a_clear_review_pass_is_not_a_fix() {
    let repo = two_value_repo();
    let server = model(vec![
        tool("read_file", json!({"path": "a.txt"})),
        tool("edit_file", json!({"path": "a.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "tests/a.sh"})),
        tool("edit_file", json!({"path": "tests/a.sh", "old_string": "-qx 1", "new_string": "-qx 2"})),
        tool("write_file", json!({"path": "tests/a_is_two.sh", "content": "grep -qx 2 a.txt\n"})),
        tool("finish", json!({
            "summary": "a.txt now holds 2.\nchanged existing tests: tests/a.sh: the issue asks a.txt to hold 2",
            "cmd": SUITE,
        })),
        json!({"text": "VERDICT: pass\nGAP:"}),
        json!({"text": "Looks plausible, I think."}),
    ])
    .await;

    let issue = "a.txt must hold 2, not 1; tests/a.sh pins the old value and must change";
    let out = slag_fix(repo.path(), &server, &[issue, "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("without the review agreeing"), "{stdout}");
}

/// The runner only runs tests/test_*.sh. The sh-backup-rotate shape: a
/// new test file it never reaches passes with or without the fix. slag
/// makes it unable to run, sees the suite still pass, and sends it back
/// with how the runner finds tests; wired in, the fix lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_the_runner_never_reaches_goes_back_until_it_is_wired_in() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
    std::fs::write(root.join("b.txt"), "1\n").unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("tests/run_tests.sh"),
        "for t in tests/test_*.sh; do bash \"$t\" || exit 1; done\n",
    )
    .unwrap();
    std::fs::write(root.join("tests/test_exists.sh"), "test -f b.txt\n").unwrap();
    sh(root, "git add -A && git commit -qm init");
    let runner = "bash tests/run_tests.sh";
    let server = model(vec![
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/b_check.sh", "content": "grep -qx 2 b.txt\n"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": runner})),
        // The revision wires the test in by the runner's own pattern.
        tool("bash", json!({"command": "mv tests/b_check.sh tests/test_b_is_two.sh"})),
        tool("finish", json!({"summary": "Renamed so the runner runs it.", "cmd": runner})),
    ])
    .await;

    let out = slag_fix(root, &server, &["b should be 2", "--test", runner]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("tests/b_check.sh never runs"), "{stdout}");
    let files = sh(root, "git show --name-only --format= HEAD");
    assert_eq!(files.trim(), "b.txt\ntests/test_b_is_two.sh", "{files}");
}

/// The issue names the command the user ran. slag runs it before any edit
/// and hands the session its output, so the first test reproduces that
/// exact call rather than one the session imagined.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_issues_own_command_runs_first_and_reaches_the_session() {
    let repo = buggy_repo();
    std::fs::write(repo.path().join("demo.sh"), "echo \"value is $(cat value.txt)\"\n").unwrap();
    sh(repo.path(), "git add demo.sh && git commit -qm demo");
    let server = model(vec![
        tool("read_file", json!({"path": "value.txt"})),
        tool("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
        tool("write_file", json!({"path": "tests/demo_says_two.sh", "content": "bash demo.sh | grep -qx 'value is 2'\n"})),
        tool("finish", json!({"summary": "value.txt:1 now holds 2.", "cmd": SUITE})),
        json!({"text": "VERDICT: pass\nGAP:"}),
    ])
    .await;

    let issue = "Running `bash demo.sh` prints value is 1; it should print value is 2";
    let out = slag_fix(repo.path(), &server, &[issue, "--test", SUITE]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("issue command: `bash demo.sh` exit 0"), "{stdout}");
    let first = server.received_requests().await.unwrap();
    let prompt = first
        .iter()
        .find(|r| r.url.path().ends_with("/chat/completions"))
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .unwrap();
    assert!(prompt.contains("THE ISSUE'S COMMAND, RUN BEFORE ANY EDIT"), "{prompt}");
    assert!(prompt.contains("value is 1"), "its real output reaches the session");
}

/// When an existing test disappears, slag puts it back itself and names
/// the exact lines, instead of asking a model that just deleted it to
/// remember what it said.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_test_is_put_back_and_its_lines_named() {
    let repo = two_value_repo();
    let suite = "def test_a():\n    assert 1\n\ndef test_b_keeps_working():\n    assert 2\n";
    std::fs::write(repo.path().join("tests/test_x.py"), suite).unwrap();
    sh(repo.path(), "git add -A && git commit -qm py");
    let server = model(vec![
        tool("read_file", json!({"path": "b.txt"})),
        tool("edit_file", json!({"path": "b.txt", "old_string": "1", "new_string": "2"})),
        tool("read_file", json!({"path": "tests/test_x.py"})),
        tool("write_file", json!({"path": "tests/test_x.py", "content": "def test_a():\n    assert 1\n"})),
        tool("finish", json!({"summary": "b.txt now holds 2.", "cmd": SUITE})),
    ])
    .await;

    let out = slag_fix(repo.path(), &server, &["b should be 2", "--test", SUITE, "--tries", "2"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("removed or skipped existing tests"), "{stdout}");
    assert_eq!(std::fs::read_to_string(repo.path().join("tests/test_x.py")).unwrap(), suite);
    // The retry's brief names the very lines that went missing.
    let retry = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .find(|body| body.contains("PREVIOUS ATTEMPT FAILED"))
        .expect("a second attempt was briefed");
    assert!(retry.contains("- def test_b_keeps_working():"), "{retry}");
}

/// SIGTERM (`timeout`, a CI cancel, `docker stop`) runs the cleanups, so a
/// command the smith started ends with slag instead of outliving it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_ends_the_command_the_smith_is_running() {
    if std::process::Command::new("pgrep").arg("-x").arg("no-such-process").output().is_err() {
        eprintln!("skipped: pgrep is not installed");
        return;
    }
    let repo = buggy_repo();
    // A duration unique to this test process, so a parallel run's sleep
    // never counts as ours.
    let sleep = format!("sleep {}", 300 + std::process::id() % 600);
    let server = model(vec![tool("bash", json!({"command": format!("touch started && {sleep}")}))]).await;
    let home = tempfile::tempdir().unwrap();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_slag"));
    for var in ["SLAG_MODEL_PLAN", "SLAG_MODEL_ALT", "SLAG_MODEL_JUDGE", "SLAG_DUEL", "SLAG_REASONING_EFFORT"] {
        cmd.env_remove(var);
    }
    let mut child = cmd
        .env("OPENROUTER_API_KEY", "sk-or-test")
        .env("SLAG_OPENROUTER_BASE", server.uri())
        .env("SLAG_MODEL_BASE", "test/base")
        .env("SLAG_CONFIG_DIR", home.path())
        .env("SLAG_SESSIONS_DIR", home.path().join("sessions"))
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .current_dir(repo.path())
        .args(["fix", "value is wrong", "--test", "bash tests/check.sh"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = repo.path().join("started");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !started.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    sh(repo.path(), &format!("kill -TERM {}", child.id()));
    let status = tokio::task::spawn_blocking(move || child.wait().unwrap()).await.unwrap();
    let alive = || std::process::Command::new("pgrep").args(["-f", &sleep]).output().unwrap().status.success();
    let mut gone = false;
    for _ in 0..50 {
        if !alive() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = std::process::Command::new("pkill").args(["-f", &sleep]).status();
    assert!(started.exists(), "the smith's command never started");
    assert_eq!(status.code(), Some(143), "slag leaves as a shell expects after SIGTERM");
    assert!(gone, "the smith's command outlived slag");
}
