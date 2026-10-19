use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verification_scratch_stays_out_of_commits_and_product_fixtures_survive() {
    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    for (name, content) in [
        ("PRD.md", "Check the supplied CSV fixture with Python.\n"),
        ("BLUEPRINT.md", "Create app.py, check.py, and fixtures/golden.csv.\n"),
        ("PROGRESS.md", "# Smithy Ledger\n"),
        (".gitignore", "# Preserve this rule\nkeep.local\nlogs/\n"),
        ("PLAN.md", "(ingot :id \"i1\" :status ore :solo nil :grade 1 :max 1 :smelt 1 :proof \"python3 check.py\" :work \"Check the supplied CSV fixture with Python\")\n"),
    ] {
        std::fs::write(root.path().join(name), content).unwrap();
    }
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "slag test"],
        vec!["config", "user.email", "forge@slag.test"],
        vec!["add", "."],
        vec!["commit", "-m", "seed commission"],
    ] {
        git(root.path(), &args);
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let inspected = body["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool");
            let (message, finish) = if inspected {
                (json!({"content": "CMD: python3 check.py"}), "stop")
            } else {
                let files = [
                    ("app.py", "VALUE = 1\n"),
                    ("fixtures/golden.csv", "name,count\none,1\n"),
                    ("check.py", "from pathlib import Path\nimport app\nassert app.VALUE == 1\nassert Path('fixtures/golden.csv').read_text() == 'name,count\\none,1\\n'\nPath('probe.csv').write_text('temporary output')\nPath('scratch').mkdir(exist_ok=True)\nPath('scratch/generated.csv').write_text('temporary output')\n"),
                ];
                let calls: Vec<_> = files.into_iter().enumerate().map(|(n, (path, content))| {
                    json!({"id": format!("write-{n}"), "type": "function", "function": {
                        "name": "write_file", "arguments": json!({"path": path, "content": content}).to_string()
                    }})
                }).collect();
                (json!({"content": null, "tool_calls": calls}), "tool_calls")
            };
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": message, "finish_reason": finish}]
            }))
        })
        .expect(2)
        .mount(&server).await;

    Command::new(env!("CARGO_BIN_EXE_slag"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("TMPDIR", std::env::temp_dir())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("SLAG_CONFIG_DIR", config.path())
        .env("SLAG_BASE_URL", server.uri())
        .env("OPENAI_API_KEY", "test-key")
        .env("SLAG_DUEL", "off")
        .env("NO_COLOR", "1")
        .args([
            "--provider",
            "openai",
            "--model",
            "test/work",
            "--plan-model",
            "test/plan",
            "--temper",
            "0",
            "resume",
        ])
        .current_dir(root.path())
        .timeout(Duration::from_secs(15))
        .assert()
        .success();

    let files = git(root.path(), &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        files.lines().any(|path| path == "fixtures/golden.csv"),
        "{files}"
    );
    assert!(files.lines().any(|path| path == "app.py"), "{files}");
    assert!(!files.contains("probe.csv"), "{files}");
    assert!(!files.contains("generated.csv"), "{files}");
    assert!(!files.contains("__pycache__"), "{files}");
    assert_eq!(
        git(root.path(), &["show", "HEAD:fixtures/golden.csv"]),
        "name,count\none,1\n"
    );
    assert!(!root.path().join("probe.csv").exists());
    assert!(!root.path().join("scratch").exists());
    let ignore = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
    assert!(
        ignore.starts_with("# Preserve this rule\nkeep.local\nlogs/\n"),
        "{ignore}"
    );
    assert!(
        ignore.lines().any(|line| line == "__pycache__/"),
        "{ignore}"
    );
    assert!(!ignore.contains("*.csv"), "{ignore}");
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_proof_repairs_fixture_writes_before_sealing_the_ingot() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let writing_test = "from pathlib import Path\nimport unittest\nclass FixtureTest(unittest.TestCase):\n    def setUp(self):\n        Path('generated-fixture.txt').write_text('sample')\n    def test_reads_fixture(self):\n        self.assertEqual(Path('generated-fixture.txt').read_text(), 'sample')\n";
    let static_test = "from pathlib import Path\nimport unittest\nclass FixtureTest(unittest.TestCase):\n    def test_reads_fixture(self):\n        self.assertEqual(Path('fixtures/input.txt').read_text(), 'sample')\n";
    for (name, content) in [
        ("PRD.md", "Read the supplied fixture with unittest.\n"),
        ("BLUEPRINT.md", "Use a static fixture and unittest.\n"),
        ("PROGRESS.md", "# Smithy Ledger\n"),
        ("BAR.md", "## THE BAR\nUse static fixtures.\nREQUIRED RUN COMMANDS: [\"python3 -m unittest\"]\n"),
        (".gitignore", "logs/\n__pycache__/\n"),
        ("PLAN.md", "(ingot :id \"i1\" :status ore :solo nil :grade 1 :max 2 :smelt 1 :proof \"python3 -m unittest\" :work \"Test the fixture without writes\")\n"),
        ("test_fixture.py", writing_test),
    ] {
        std::fs::write(root.path().join(name), content).unwrap();
    }
    std::fs::create_dir(root.path().join("fixtures")).unwrap();
    std::fs::write(root.path().join("fixtures/input.txt"), "sample").unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "slag test"],
        vec!["config", "user.email", "forge@slag.test"],
        vec!["add", "."],
        vec!["commit", "-m", "seed fixture test"],
    ] {
        git(root.path(), &args);
    }
    let server = MockServer::start().await;
    let calls = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let n = calls.fetch_add(1, Ordering::SeqCst);
            let (message, finish) = match n {
                0 => (json!({"content": null, "tool_calls": [{"id": "save-progress", "type": "function", "function": {
                    "name": "write_file", "arguments": json!({"path": "build-progress.txt", "content": "done\n"}).to_string()
                }}]}), "tool_calls"),
                1 | 4 => (json!({"content": "CMD: python3 -m unittest"}), "stop"),
                2 => {
                    assert!(body["messages"].to_string().contains("generated-fixture.txt"));
                    (json!({"content": null, "tool_calls": [
                        {"id": "read-progress", "type": "function", "function": {
                            "name": "read_file", "arguments": json!({"path": "build-progress.txt"}).to_string()
                        }},
                        {"id": "read-test", "type": "function", "function": {
                            "name": "read_file", "arguments": json!({"path": "test_fixture.py"}).to_string()
                        }},
                    ]}), "tool_calls")
                }
                3 => {
                    let progress = body["messages"].as_array().unwrap().iter()
                        .find(|message| message["role"] == "tool" && message["tool_call_id"] == "read-progress")
                        .unwrap()["content"].as_str().unwrap();
                    assert!(!progress.starts_with("ERROR:"), "{progress}");
                    assert!(progress.contains("done"), "{progress}");
                    (json!({"content": null, "tool_calls": [{"id": "fix-test", "type": "function", "function": {
                        "name": "write_file", "arguments": json!({"path": "test_fixture.py", "content": static_test}).to_string()
                    }}]}), "tool_calls")
                }
                _ => panic!("unexpected extra model request {n}"),
            };
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": message, "finish_reason": finish}]
            }))
        })
        .expect(5)
        .mount(&server).await;

    Command::new(env!("CARGO_BIN_EXE_slag"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("TMPDIR", std::env::temp_dir())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("SLAG_CONFIG_DIR", config.path())
        .env("SLAG_BASE_URL", server.uri())
        .env("OPENAI_API_KEY", "test-key")
        .env("SLAG_DUEL", "off")
        .env("NO_COLOR", "1")
        .args([
            "--provider",
            "openai",
            "--model",
            "test/work",
            "--plan-model",
            "test/plan",
            "--temper",
            "0",
            "resume",
        ])
        .current_dir(root.path())
        .timeout(Duration::from_secs(15))
        .assert()
        .success();

    let plan = git(root.path(), &["show", "HEAD:PLAN.md"]);
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 2"), "{plan}");
    assert_eq!(
        git(root.path(), &["show", "HEAD:test_fixture.py"]),
        static_test
    );
    assert_eq!(
        git(root.path(), &["show", "HEAD:build-progress.txt"]),
        "done\n"
    );
    assert!(!root.path().join("generated-fixture.txt").exists());
    let failed = std::fs::read_dir(root.path().join("logs"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains("_ASSAY_i1_1.log")
        })
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .unwrap();
    assert!(failed.contains("exit=1"), "{failed}");
    assert!(failed.contains("PermissionError") || failed.contains("Read-only file system"), "{failed}");
    assert!(git(root.path(), &["status", "--porcelain"]).is_empty());
    server.verify().await;
}
