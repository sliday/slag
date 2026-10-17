use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COMMISSION: &str = "write wc.py that counts words in a file, with a unittest";
const PROOF: &str = "python3 -m unittest";
const SOURCE: &str = "import sys\n\ndef count_words(path):\n    with open(path, encoding='utf-8') as source:\n        return len(source.read().split())\n\nif __name__ == '__main__':\n    print(count_words(sys.argv[1]))\n";
const TEST: &str = "import tempfile\nimport unittest\nimport wc\n\nclass WordCountTest(unittest.TestCase):\n    def test_counts_words(self):\n        with tempfile.NamedTemporaryFile(mode='w+', encoding='utf-8') as source:\n            source.write('one two\\nthree')\n            source.flush()\n            self.assertEqual(wc.count_words(source.name), 3)\n";

fn command(root: &Path, config: &Path, server: &MockServer) -> Command {
    let mut cmd = base_command(root, config, server);
    cmd.args(["--temper", "1", COMMISSION]);
    cmd
}

fn base_command(root: &Path, config: &Path, server: &MockServer) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slag"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "slag test")
        .env("GIT_AUTHOR_EMAIL", "forge@slag.test")
        .env("GIT_COMMITTER_NAME", "slag test")
        .env("GIT_COMMITTER_EMAIL", "forge@slag.test")
        .env("SLAG_CONFIG_DIR", config)
        .env("SLAG_BASE_URL", server.uri())
        .env("OPENAI_API_KEY", "test-key")
        .env("SLAG_DUEL", "off")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("NO_COLOR", "1")
        .args([
            "--provider",
            "openai",
            "--model",
            "test/work",
            "--plan-model",
            "test/plan",
        ])
        .current_dir(root)
        .timeout(Duration::from_secs(15));
    cmd
}

fn tool(id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "id": id, "type": "function",
        "function": {"name": name, "arguments": arguments.to_string()}
    })
}

fn response(message: Value) -> ResponseTemplate {
    let finish = if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 25, "total_tokens": 125}
    }))
}

fn tool_results(body: &Value) -> Vec<&str> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap())
        .collect()
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_commission_protects_coordinator_state_and_seals_before_review() {
    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let messages = body["messages"].to_string();
            let results = tool_results(&body);
            if messages.contains("Choose the work scope") {
                assert!(results.is_empty());
                return response(json!({"content": json!({"direct": true, "proof": PROOF}).to_string()}));
            }
            if body["model"] == "test/work" {
                if results.is_empty() {
                    let mut calls = vec![
                        tool("source", "write_file", json!({"path": "wc.py", "content": SOURCE})),
                        tool("tests", "write_file", json!({"path": "test_wc.py", "content": TEST})),
                    ];
                    for name in ["PLAN.md", "PROGRESS.md", "BLUEPRINT.md", "BAR.md", "PRD.md", "AGENTS.md", "logs/attack.log"] {
                        calls.push(tool(name, "write_file", json!({"path": name, "content": "stale coordinator state"})));
                    }
                    calls.push(tool("restore", "bash", json!({"command": "git restore PLAN.md PROGRESS.md"})));
                    let stale = "(ingot :id \"i1\" :status ore :solo nil :grade 1 :heat 0 :max 3 :proof \"python3 -m unittest\" :work \"Count words\")";
                    let code = format!(
                        "from pathlib import Path; Path(\"PLAN\" + \".md\").write_text({}); Path(\"PROGRESS\" + \".md\").write_text(\"stale ledger\")",
                        serde_json::to_string(stale).unwrap()
                    );
                    calls.push(tool("indirect", "bash", json!({"command": format!("python3 -c '{code}'")})));
                    return response(json!({"content": null, "tool_calls": calls}));
                }
                assert_eq!(results.len(), 11, "{results:?}");
                assert!(results[2..9].iter().all(|r| r.contains("slag owns")), "{results:?}");
                assert!(results[9].contains("do not restore"), "{results:?}");
                assert!(results[10].contains("slag restored its coordinator files"), "{results:?}");
                return response(json!({"content": format!("CMD: {PROOF}")}));
            }
            assert!(messages.contains("# slag warden"), "unexpected preparation phase: {messages}");
            let names: Vec<_> = body["tools"].as_array().unwrap().iter()
                .map(|spec| spec["function"]["name"].as_str().unwrap()).collect();
            assert!(!names.contains(&"write_file"), "{names:?}");
            assert!(!names.contains(&"edit_file"), "{names:?}");
            assert!(!names.contains(&"finish"), "{names:?}");
            if results.is_empty() {
                return response(json!({"content": null, "tool_calls": [
                    tool("review-write", "write_file", json!({"path": "wc.py", "content": "raise RuntimeError('review edited product')"})),
                    tool("inspect", "read_file", json!({"path": "wc.py"})),
                    tool("verify", "bash", json!({"command": "python3 -m unittest && test -z \"$(git status --porcelain)\" && git show HEAD:PLAN.md | grep -q ':status forged' && git show HEAD:PROGRESS.md | grep -q 'heats:1' && printf 'sealed and checked'"})),
                ]}));
            }
            assert_eq!(results.len(), 3, "{results:?}");
            assert!(results[0].contains("unavailable during review"), "{results:?}");
            assert!(results[1].contains("def count_words(path)"), "{results:?}");
            assert!(results[2].contains("sealed and checked"), "{results:?}");
            response(json!({"content": "VERDICT: pass\nEVIDENCE: read wc.py, ran unittest, checked committed forged status and clean tree"}))
        })
        .expect(5)
        .mount(&server).await;

    command(root.path(), config.path(), &server)
        .assert()
        .success()
        .stdout(contains("FORGED"));
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert_eq!(plan.matches("(ingot ").count(), 1, "{plan}");
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 1"), "{plan}");
    assert_eq!(git(root.path(), &["show", "HEAD:PLAN.md"]), plan);
    let ledger = git(root.path(), &["show", "HEAD:PROGRESS.md"]);
    assert_eq!(ledger.matches("heats:1").count(), 1, "{ledger}");
    assert!(ledger.contains("Warden round 1"), "{ledger}");
    assert!(ledger.contains("goal met"), "{ledger}");
    assert!(!ledger.contains("stale ledger"), "{ledger}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
        "## Alloy Recipes\n"
    );
    assert!(!root.path().join("logs/attack.log").exists());
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_commission_keeps_the_final_warden_failure() {
    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let results = tool_results(&body);
            if body["messages"].to_string().contains("Choose the work scope") {
                response(json!({"content": json!({"direct": true, "proof": PROOF}).to_string()}))
            } else if body["model"] == "test/work" {
                if results.is_empty() {
                    response(json!({"content": null, "tool_calls": [
                        tool("source", "write_file", json!({"path": "wc.py", "content": "print(0)\n"})),
                        tool("tests", "write_file", json!({"path": "test_wc.py", "content": "import unittest\nclass Placeholder(unittest.TestCase):\n    def test_trivial(self):\n        self.assertTrue(True)\n"})),
                    ]}))
                } else {
                    response(json!({"content": format!("CMD: {PROOF}")}))
                }
            } else if results.is_empty() {
                response(json!({"content": null, "tool_calls": [
                    tool("inspect", "read_file", json!({"path": "wc.py"})),
                ]}))
            } else {
                assert!(results[0].contains("print(0)"), "{results:?}");
                response(json!({"content": "VERDICT: fail\nGAP: wc.py prints zero without reading the supplied file\nEVIDENCE: read wc.py; its only statement is print(0)"}))
            }
        })
        .expect(5)
        .mount(&server).await;

    command(root.path(), config.path(), &server)
        .assert()
        .failure()
        .stdout(contains("GOAL NOT MET"))
        .stderr(contains("wc.py prints zero"));
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 1"), "{plan}");
    let ledger = git(root.path(), &["show", "HEAD:PROGRESS.md"]);
    assert!(ledger.contains("goal not met"), "{ledger}");
    assert!(ledger.contains("wc.py prints zero"), "{ledger}");
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
    server.verify().await;
}

fn resumed_project(proof: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (name, content) in [
        ("PRD.md", "Write the result to artifact.txt.\n".to_string()),
        ("BLUEPRINT.md", "Create artifact.txt.\n".to_string()),
        ("PROGRESS.md", "# Smithy Ledger\n".to_string()),
        (".gitignore", "logs/\n".to_string()),
        ("PLAN.md", format!(
            "(ingot :id \"i1\" :status ore :solo nil :grade 1 :heat 2 :max 3 :smelt 1 :proof \"{proof}\" :work \"Create artifact.txt\")\n"
        )),
    ] {
        std::fs::write(root.path().join(name), content).unwrap();
    }
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "slag test"],
        vec!["config", "user.email", "forge@slag.test"],
        vec!["add", "."],
        vec!["commit", "-m", "interrupted after two heats"],
    ] {
        git(root.path(), &args);
    }
    root
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_single_smith_records_cumulative_heat_without_extra_attempts() {
    let root = resumed_project("test -f artifact.txt");
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["model"], "test/work");
            if tool_results(&body).is_empty() {
                response(json!({"content": null, "tool_calls": [
                    tool("artifact", "write_file", json!({"path": "artifact.txt", "content": "third heat passed\n"})),
                ]}))
            } else {
                response(json!({"content": "CMD: test -f artifact.txt"}))
            }
        })
        .expect(2)
        .mount(&server).await;

    base_command(root.path(), config.path(), &server)
        .args(["--temper", "0", "resume"])
        .assert()
        .success()
        .stdout(contains("FORGED"));
    let plan = git(root.path(), &["show", "HEAD:PLAN.md"]);
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 3"), "{plan}");
    let ledger = git(root.path(), &["show", "HEAD:PROGRESS.md"]);
    assert_eq!(ledger.matches("heats:3").count(), 1, "{ledger}");
    assert!(!ledger.contains("heats:1"), "{ledger}");
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_single_smith_preserves_debris_after_its_final_failed_heat() {
    let root = resumed_project("false");
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["model"], "test/work");
            if tool_results(&body).is_empty() {
                response(json!({"content": null, "tool_calls": [
                    tool("artifact", "write_file", json!({"path": "artifact.txt", "content": "inspect this final failed attempt\n"})),
                ]}))
            } else {
                response(json!({"content": "CMD: false"}))
            }
        })
        .expect(2)
        .mount(&server).await;

    base_command(root.path(), config.path(), &server)
        .args(["--temper", "0", "resume"])
        .assert()
        .failure()
        .stderr(contains("1 ingots cracked"));
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert!(plan.contains(":status cracked"), "{plan}");
    assert!(plan.contains(":heat 3"), "{plan}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("artifact.txt")).unwrap(),
        "inspect this final failed attempt\n"
    );
    let ledger = std::fs::read_to_string(root.path().join("PROGRESS.md")).unwrap();
    assert!(!ledger.contains("heats:"), "{ledger}");
    server.verify().await;
}
