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
const TEST: &str = "import unittest\nfrom unittest.mock import mock_open, patch\nimport wc\n\nclass WordCountTest(unittest.TestCase):\n    def test_counts_words(self):\n        with patch('builtins.open', mock_open(read_data='one two\\nthree')):\n            self.assertEqual(wc.count_words('sample.txt'), 3)\n";

fn command(root: &Path, config: &Path, server: &MockServer) -> Command {
    let mut cmd = base_command(root, config, server);
    cmd.args(["--temper", "1", COMMISSION]);
    cmd
}

fn base_command(root: &Path, config: &Path, server: &MockServer) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slag"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("TMPDIR", std::env::temp_dir())
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
    let review_root = root.path().to_path_buf();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let messages = body["messages"].to_string();
            let results = tool_results(&body);
            if messages.contains("Choose the work scope") {
                assert!(results.is_empty());
                return response(json!({"content": direct_scope().to_string()}));
            }
            if body["model"] == "test/work" {
                if results.is_empty() {
                    let mut calls = vec![
                        tool("source", "write_file", json!({"path": "wc.py", "content": SOURCE})),
                        tool("tests", "write_file", json!({"path": "test_wc.py", "content": TEST})),
                    ];
                    for name in [".slag/PLAN.md", ".slag/PROGRESS.md", ".slag/BLUEPRINT.md", ".slag/BAR.md", ".slag/PRD.md", ".slag/AGENTS.md", ".slag/logs/attack.log"] {
                        calls.push(tool(name, "write_file", json!({"path": name, "content": "stale coordinator state"})));
                    }
                    calls.push(tool("restore", "bash", json!({"command": "git restore .slag/PLAN.md .slag/PROGRESS.md"})));
                    let stale = "(ingot :id \"i1\" :status ore :solo nil :grade 1 :heat 0 :max 3 :proof \"python3 -m unittest\" :work \"Count words\")";
                    let code = format!(
                        "from pathlib import Path; (Path(\".\" + \"slag\") / (\"PLAN\" + \".md\")).write_text({}); (Path(\".\" + \"slag\") / (\"PROGRESS\" + \".md\")).write_text(\"stale ledger\")",
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
                assert!(git(&review_root, &["status", "--porcelain"]).is_empty());
                assert!(std::fs::read_to_string(review_root.join(".slag/PLAN.md")).unwrap().contains(":status forged"));
                assert!(std::fs::read_to_string(review_root.join(".slag/PROGRESS.md")).unwrap().contains("heats:1"));
                return response(json!({"content": null, "tool_calls": ([
                    tool("inspect", "read_file", json!({"path": "wc.py"})),
                    tool("inspect-tests", "read_file", json!({"path": "test_wc.py"})),
                ].into_iter().chain(required_runs(&body)).collect::<Vec<_>>())}));
            }
            assert!(results.len() >= 3, "{results:?}");
            assert!(results[0].contains("def count_words(path)"), "{results:?}");
            assert!(results[1].contains("assertEqual(wc.count_words"), "{results:?}");
            assert!(results.iter().all(|output| !output.starts_with("ERROR:")), "{results:?}");
            grounded_pass(&body)
        })
        .expect(5)
        .mount(&server).await;

    command(root.path(), config.path(), &server)
        .assert()
        .success()
        .stdout(contains("FORGED"));
    let plan = std::fs::read_to_string(root.path().join(".slag/PLAN.md")).unwrap();
    assert_eq!(plan.matches("(ingot ").count(), 1, "{plan}");
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 1"), "{plan}");
    let tracked = git(root.path(), &["ls-files"]);
    assert!(!tracked.lines().any(|path| path.starts_with(".slag/") || matches!(path, "PLAN.md" | "PROGRESS.md" | "PRD.md" | "BLUEPRINT.md" | "BAR.md" | "AGENTS.md")), "{tracked}");
    let ledger = std::fs::read_to_string(root.path().join(".slag/PROGRESS.md")).unwrap();
    assert_eq!(ledger.matches("heats:1").count(), 1, "{ledger}");
    assert!(ledger.contains("Warden round 1"), "{ledger}");
    assert!(ledger.contains("goal met"), "{ledger}");
    assert!(!ledger.contains("stale ledger"), "{ledger}");
    assert_eq!(
        std::fs::read_to_string(root.path().join(".slag/AGENTS.md")).unwrap(),
        "## Alloy Recipes\n"
    );
    assert!(!root.path().join(".slag/logs/attack.log").exists());
    let status = Command::new(env!("CARGO_BIN_EXE_slag"))
        .current_dir(root.path()).args(["status", "--json"]).assert().success().get_output().stdout.clone();
    let status: Value = serde_json::from_slice(&status).unwrap();
    assert_eq!(status["crucible"], true);
    assert_eq!(status["ingots"]["forged"], 1);
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
                response(json!({"content": direct_scope().to_string()}))
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
                response(json!({"content": "VERDICT: fail\nGAP: wc.py prints zero without reading the supplied file\nEVIDENCE: [\"inspect\"]"}))
            }
        })
        .expect(5)
        .mount(&server).await;

    command(root.path(), config.path(), &server)
        .assert()
        .failure()
        .stdout(contains("GOAL NOT MET"))
        .stderr(contains("wc.py prints zero"));
    let plan = std::fs::read_to_string(root.path().join(".slag/PLAN.md")).unwrap();
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 1"), "{plan}");
    let ledger = std::fs::read_to_string(root.path().join(".slag/PROGRESS.md")).unwrap();
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

const INSTALL_GAP: &str =
    "PEP668 blocks the documented pip install command; use a virtual environment.";
const INSTALL_BEFORE: &str = "# Install\n\npip install example\n";
const INSTALL_AFTER: &str =
    "# Install\n\npython3 -m venv .venv\n. .venv/bin/activate\npip install example\n";

fn forged_project(gap: Option<&str>) -> tempfile::TempDir {
    let root = resumed_project("test -f README.md");
    for (name, content) in [
        ("PRD.md", "# Commission\n\nProvide install instructions that work under PEP668.\n".to_string()),
        ("BLUEPRINT.md", "# Blueprint\n\nDocument installation in README.md.\n".to_string()),
        ("BAR.md", "## THE BAR\nHonor the commission.\n- Installation works under PEP668.\n".to_string()),
        ("README.md", INSTALL_BEFORE.to_string()),
        ("PLAN.md", "(ingot :id \"i1\" :status forged :solo nil :grade 1 :heat 1 :max 3 :proof \"test -f README.md\" :work \"Write install instructions\")\n".to_string()),
        ("PROGRESS.md", gap.map_or_else(
            || "# Smithy Ledger\n".to_string(),
            |gap| format!("# Smithy Ledger\n\n## Warden round 1 — goal not met\n\n- gap: {gap}\n- evidence: pip install rejected the externally managed environment\n")
        )),
    ] {
        std::fs::write(root.path().join(name), content).unwrap();
    }
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "artifact awaits tempering"]);
    root
}

fn artifact_review(body: &Value) -> bool {
    body["messages"].as_array().unwrap().iter().any(|m| {
        m["role"] == "system"
            && m["content"]
                .as_str()
                .is_some_and(|s| s.contains("# slag warden"))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn temper_resume_forges_the_prior_gap_before_the_next_artifact_review() {
    assert_resume_repairs_gap(&["--temper", "2", "resume"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_resume_cannot_erase_a_prior_rejection() {
    assert_resume_repairs_gap(&["resume"]).await;
}

async fn assert_resume_repairs_gap(args: &[&str]) {
    use std::sync::{Arc, Mutex};

    let root = forged_project(Some(INSTALL_GAP));
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let seen = phases.clone();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let messages = body["messages"].to_string();
            let results = tool_results(&body);
            if artifact_review(&body) {
                if results.is_empty() {
                    seen.lock().unwrap().push("warden-inspect");
                    response(json!({"content": null, "tool_calls": ([
                        tool("inspect", "read_file", json!({"path": "README.md"})),
                    ].into_iter().chain(required_runs(&body)).collect::<Vec<_>>())}))
                } else if results[0].contains("python3 -m venv") {
                    seen.lock().unwrap().push("warden-pass");
                    grounded_pass(&body)
                } else {
                    seen.lock().unwrap().push("warden-fail-before-repair");
                    response(json!({"content": format!("VERDICT: fail\nGAP: {INSTALL_GAP}\nEVIDENCE: [\"inspect\"]")}))
                }
            } else if messages.contains("ROLE: Master Founder") {
                seen.lock().unwrap().push("founder");
                assert!(messages.contains(INSTALL_GAP), "{messages}");
                response(json!({"content": "(ingot :id \"i1\" :status ore :solo nil :grade 1 :max 1 :proof \"grep -q venv README.md\" :work \"Fix README.md installation under PEP668 with a virtual environment\")"}))
            } else if body["model"] == "test/work" {
                if results.is_empty() {
                    seen.lock().unwrap().push("smith-read");
                    response(json!({"content": null, "tool_calls": [
                        tool("read-install", "read_file", json!({"path": "README.md"})),
                    ]}))
                } else if body["messages"].as_array().unwrap().iter().rev()
                    .find(|message| message["role"] == "tool")
                    .is_some_and(|message| message["tool_call_id"] == "read-install") {
                    seen.lock().unwrap().push("smith-write");
                    response(json!({"content": null, "tool_calls": [
                        tool("fix-install", "write_file", json!({"path": "README.md", "content": INSTALL_AFTER})),
                    ]}))
                } else {
                    seen.lock().unwrap().push("smith-finish");
                    response(json!({"content": "CMD: grep -q venv README.md"}))
                }
            } else {
                seen.lock().unwrap().push("checkpoint");
                plan_review_response(&body)
            }
        })
        .mount(&server).await;

    let output = base_command(root.path(), config.path(), &server)
        .args(args)
        .assert()
        .success()
        .get_output()
        .clone();
    let observed = phases.lock().unwrap();
    assert!(observed.contains(&"founder"), "{observed:?}\n{output:?}");
    assert!(
        !observed.contains(&"warden-fail-before-repair"),
        "{observed:?}"
    );
    let positions: Vec<_> = [
        "founder",
        "smith-read",
        "smith-write",
        "smith-finish",
        "warden-inspect",
        "warden-pass",
    ]
    .iter()
    .map(|phase| observed.iter().position(|entry| entry == phase).unwrap())
    .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{observed:?}"
    );
    assert_eq!(
        observed.iter().filter(|&&p| p == "founder").count(),
        1,
        "{observed:?}"
    );
    assert_eq!(
        observed.iter().filter(|&&p| p == "warden-inspect").count(),
        1,
        "{observed:?}"
    );
    let plan = git(root.path(), &["show", "HEAD:PLAN.md"]);
    assert_eq!(plan.matches(":status forged").count(), 2, "{plan}");
    assert!(plan.contains(":id \"i2\""), "{plan}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("README.md")).unwrap(),
        INSTALL_AFTER
    );
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_artifact_verdict_reports_an_unverified_goal() {
    let root = forged_project(None);
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if !artifact_review(&body) {
                return plan_review_response(&body);
            }
            if tool_results(&body).is_empty() {
                response(json!({"content": null, "tool_calls": [
                    tool("inspect", "read_file", json!({"path": "README.md"})),
                ]}))
            } else {
                response(json!({"content": "Inspection complete. I read README.md."}))
            }
        })
        .mount(&server)
        .await;

    let output = base_command(root.path(), config.path(), &server)
        .args(["--temper", "2", "resume"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let display = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        display.to_lowercase().contains("goal unverified"),
        "{display}"
    );
    assert!(
        !display.to_lowercase().contains("goal not met"),
        "{display}"
    );
    assert!(!display.contains("FORGED"), "{display}");
    assert!(!display.to_lowercase().contains("rejected"), "{display}");
    assert!(!display.to_lowercase().contains("accepted"), "{display}");
    let ledger = std::fs::read_to_string(root.path().join("PROGRESS.md")).unwrap();
    assert!(ledger.contains("goal unverified"), "{ledger}");
    assert!(!ledger.contains("goal not met"), "{ledger}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| {
                if !r.url.path().ends_with("/chat/completions") {
                    return false;
                }
                let body: Value = serde_json::from_slice(&r.body).unwrap();
                artifact_review(&body)
            })
            .count(),
        2
    );
    assert_eq!(
        git(root.path(), &["show", "HEAD:PLAN.md"])
            .matches("(ingot ")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forbidden_warden_mutations_cannot_produce_an_accepted_pass() {
    for attack in [
        tool(
            "edit",
            "edit_file",
            json!({"path": "README.md", "old_string": "pip install example", "new_string": "skip installation"}),
        ),
        tool(
            "touch",
            "bash",
            json!({"command": "touch review-marker.txt"}),
        ),
    ] {
        let root = forged_project(None);
        let config = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                if !artifact_review(&body) {
                    return plan_review_response(&body);
                }
                if tool_results(&body).is_empty() {
                    response(json!({"content": null, "tool_calls": [
                        tool("inspect", "read_file", json!({"path": "README.md"})),
                        attack.clone(),
                    ]}))
                } else {
                    response(json!({"content": "VERDICT: pass\nEVIDENCE: [\"inspect\"]"}))
                }
            })
            .mount(&server)
            .await;

        let output = base_command(root.path(), config.path(), &server)
            .args(["--temper", "1", "resume"])
            .assert()
            .failure()
            .get_output()
            .clone();
        let display = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            display.to_lowercase().contains("goal unverified"),
            "{display}"
        );
        assert!(!display.contains("FORGED"), "{display}");
        assert!(!display.contains("goal met"), "{display}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("README.md")).unwrap(),
            INSTALL_BEFORE
        );
        assert!(!root.path().join("review-marker.txt").exists());
        let ledger = std::fs::read_to_string(root.path().join("PROGRESS.md")).unwrap();
        assert!(ledger.contains("goal unverified"), "{ledger}");
        assert!(!ledger.contains("goal met"), "{ledger}");
    }
}

fn plan_review_response(body: &Value) -> ResponseTemplate {
    if tool_results(body).is_empty() {
        response(json!({"content": null, "tool_calls": [
            tool("plan-read", "read_file", json!({"path": "BLUEPRINT.md"})),
        ]}))
    } else {
        response(json!({"content": "VERDICT: pass\nEVIDENCE: [\"plan-read\"]"}))
    }
}

fn evidence_project() -> tempfile::TempDir {
    let root = forged_project(None);
    std::fs::write(root.path().join("README.md"), INSTALL_AFTER).unwrap();
    git(root.path(), &["add", "README.md"]);
    git(
        root.path(),
        &["commit", "-m", "document isolated installation"],
    );
    root
}

fn assert_unverified_review(output: &std::process::Output) {
    let display = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "{display}");
    assert!(
        display.to_lowercase().contains("goal unverified"),
        "{display}"
    );
    assert!(!display.contains("FORGED"), "{display}");
    assert!(!display.to_lowercase().contains("accepted"), "{display}");
    assert!(!display.to_lowercase().contains("rejected"), "{display}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_read_cannot_ground_failed_unknown_or_prose_evidence() {
    for evidence in [
        r#"["read-failed"]"#,
        r#"["not-called"]"#,
        "read-ok inspected missing.py and confirmed it passed",
    ] {
        let root = evidence_project();
        let config = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                if !artifact_review(&body) {
                    return plan_review_response(&body);
                }
                let results = tool_results(&body);
                if results.is_empty() {
                    response(json!({"content": null, "tool_calls": [
                        tool("read-ok", "read_file", json!({"path": "README.md"})),
                        tool("read-failed", "read_file", json!({"path": "missing.py"})),
                    ]}))
                } else {
                    assert_eq!(results.len(), 2, "{results:?}");
                    assert!(results[0].contains("python3 -m venv"), "{results:?}");
                    assert!(results[1].starts_with("ERROR:"), "{results:?}");
                    response(
                        json!({"content": format!("VERDICT: pass\nGAP:\nEVIDENCE: {evidence}")}),
                    )
                }
            })
            .mount(&server)
            .await;

        let output = base_command(root.path(), config.path(), &server)
            .args(["--temper", "1", "resume"])
            .assert()
            .get_output()
            .clone();
        assert_unverified_review(&output);
        let ledger = git(root.path(), &["show", "HEAD:PROGRESS.md"]);
        assert!(
            ledger.contains("goal unverified"),
            "evidence={evidence}: {ledger}"
        );
        assert!(
            !ledger.contains("goal met"),
            "evidence={evidence}: {ledger}"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("README.md")).unwrap(),
            INSTALL_AFTER
        );
        assert!(!root.path().join("missing.py").exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_checks_ground_a_pass_with_the_actual_inspected_path() {
    let root = evidence_project();
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if !artifact_review(&body) {
                return plan_review_response(&body);
            }
            let results = tool_results(&body);
            if results.is_empty() {
                response(json!({"content": null, "tool_calls": ([
                    tool("read-ok", "read_file", json!({"path": "README.md"})),
                ].into_iter().chain(required_runs(&body)).collect::<Vec<_>>())}))
            } else {
                assert!(results[0].contains("python3 -m venv"), "{results:?}");
                grounded_pass(&body)
            }
        })
        .mount(&server)
        .await;

    base_command(root.path(), config.path(), &server)
        .args(["--temper", "1", "resume"])
        .assert()
        .success()
        .stdout(contains("FORGED"));
    let ledger = git(root.path(), &["show", "HEAD:PROGRESS.md"]);
    assert!(ledger.contains("goal met"), "{ledger}");
    assert!(ledger.contains("README.md"), "{ledger}");
    assert!(ledger.contains("read_file"), "{ledger}");
    assert!(!ledger.contains("goal unverified"), "{ledger}");
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_rejudges_unverified_work_and_requires_current_grounded_evidence() {
    let root = evidence_project();
    let config = tempfile::tempdir().unwrap();
    let original_plan = git(root.path(), &["show", "HEAD:PLAN.md"]);
    for attempt in 0..3 {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                if !artifact_review(&body) {
                    return plan_review_response(&body);
                }
                let results = tool_results(&body);
                if results.is_empty() {
                    response(json!({"content": null, "tool_calls": ([
                        tool(&format!("read-{attempt}"), "read_file", json!({"path": "README.md"})),
                    ].into_iter().chain(if attempt == 2 { required_runs(&body) } else { Vec::new() }).collect::<Vec<_>>())}))
                } else {
                    assert!(results[0].contains("python3 -m venv"), "{results:?}");
                    let verdict = match attempt {
                        0 => "VERDICT: unverified\nGAP: inspection stopped before a verdict\nEVIDENCE: [\"read-0\"]".to_string(),
                        1 => "VERDICT: pass\nGAP:\nEVIDENCE: [\"read-0\"]".to_string(),
                        _ => format!("VERDICT: pass\nGAP:\nEVIDENCE: {}", inspection_ids(&body)),
                    };
                    response(json!({"content": verdict}))
                }
            })
            .mount(&server).await;

        let output = base_command(root.path(), config.path(), &server)
            .args(if attempt == 0 {
                &["--temper", "1", "resume"][..]
            } else {
                &["resume"][..]
            })
            .assert()
            .get_output()
            .clone();
        if attempt < 2 {
            assert_unverified_review(&output);
        } else {
            assert!(output.status.success(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("FORGED"),
                "{output:?}"
            );
        }
        assert_eq!(git(root.path(), &["show", "HEAD:PLAN.md"]), original_plan);
        assert_eq!(
            std::fs::read_to_string(root.path().join("README.md")).unwrap(),
            INSTALL_AFTER
        );
        let requests = server.received_requests().await.unwrap();
        let reviews: Vec<Value> = requests
            .iter()
            .filter(|r| r.url.path() == "/chat/completions")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert!(
            reviews.iter().all(|body| body["model"] == "test/plan"),
            "{reviews:?}"
        );
        assert_eq!(
            reviews.iter().filter(|body| artifact_review(body)).count(),
            2,
            "{reviews:?}"
        );
        assert!(!reviews.iter().any(|body| body["messages"]
            .to_string()
            .contains("ROLE: Master Founder")));
    }
    let ledger = git(root.path(), &["show", "HEAD:PROGRESS.md"]);
    let latest = ledger.rsplit("## Warden round ").next().unwrap();
    assert!(
        latest.lines().next().unwrap().ends_with("goal met"),
        "{ledger}"
    );
    assert!(latest.contains("README.md"), "{ledger}");
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
}

fn direct_scope() -> Value {
    json!({
        "direct": true,
        "proof": PROOF,
        "criteria": [
            "wc.py counts words in the supplied file.",
            "python3 wc.py FILE prints the word count.",
            "unittest checks word counts with in-memory inputs."
        ],
        "commands": ["python3 wc.py test_wc.py"]
    })
}

fn required_runs(body: &Value) -> Vec<Value> {
    let commands: Vec<String> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .flat_map(str::lines)
        .find_map(|line| line.strip_prefix("REQUIRED RUN COMMANDS: "))
        .map(|commands| serde_json::from_str(commands).unwrap())
        .expect("artifact review must declare its required commands");
    assert!(!commands.is_empty(), "{body}");
    commands
        .into_iter()
        .enumerate()
        .map(|(n, command)| tool(&format!("check-{n}"), "bash", json!({"command": command})))
        .collect()
}

fn inspection_ids(body: &Value) -> Value {
    let ids: Vec<_> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter(|message| !message["content"].as_str().unwrap().starts_with("ERROR:"))
        .map(|message| message["tool_call_id"].as_str().unwrap())
        .collect();
    json!(ids)
}

fn grounded_pass(body: &Value) -> ResponseTemplate {
    response(json!({"content": format!("VERDICT: pass\nGAP:\nEVIDENCE: {}", inspection_ids(body))}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_inspection_without_the_declared_run_cannot_pass() {
    let root = evidence_project();
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if !artifact_review(&body) {
                return plan_review_response(&body);
            }
            if tool_results(&body).is_empty() {
                response(json!({"content": null, "tool_calls": [
                    tool("read-only", "read_file", json!({"path": "README.md"})),
                ]}))
            } else {
                grounded_pass(&body)
            }
        })
        .mount(&server)
        .await;

    let output = base_command(root.path(), config.path(), &server)
        .args(["--temper", "1", "resume"])
        .assert()
        .get_output()
        .clone();
    assert_unverified_review(&output);
    let ledger = git(root.path(), &["show", "HEAD:PROGRESS.md"]);
    assert!(ledger.contains("goal unverified"), "{ledger}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_reference_contract_stops_before_forging() {
    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let invalid = json!({
        "direct": true, "proof": "python3 -m unittest", "criteria": ["Copy file contents like cat."],
        "commands": ["python3 copy.py fixtures/sample.txt"]
    }).to_string();
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": invalid}, "finish_reason": "stop"}]
        }))).expect(2).mount(&server).await;
    let output = base_command(root.path(), config.path(), &server)
        .args(["--temper", "1", "A tiny file-copy CLI like cat with a unittest suite."])
        .assert().failure().get_output().clone();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("FORGED"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no valid comparison contract"));
    assert!(!root.path().join("copy.py").exists());
    assert!(!root.path().join("PLAN.md").exists());
    server.verify().await;
}
