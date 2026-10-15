use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use predicates::str::contains;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn project(plan: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in [
        ("PRD.md", "Print hello.\n"),
        ("BLUEPRINT.md", "Create hello.py.\n"),
        ("PLAN.md", plan),
    ] {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "forge@slag.test"],
        vec!["config", "user.name", "slag"],
        vec!["commit", "--allow-empty", "-m", "base"],
    ] {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
    dir
}

fn forge(root: &Path, config: &Path, base: &str, duel: &str) -> Command {
    let mut cmd = command(root, config, base, duel);
    cmd.arg("resume");
    cmd
}

fn command(root: &Path, config: &Path, base: &str, duel: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slag"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("OPENROUTER_API_KEY", "sk-or-test")
        .env("SLAG_CONFIG_DIR", config)
        .env("SLAG_OPENROUTER_BASE", base)
        .env("SLAG_MODEL_BASE", "test/base")
        .env("SLAG_MODEL_PLAN", "test/plan")
        .env("SLAG_MODEL_ALT", "test/alt")
        .env("SLAG_DUEL", duel)
        .env("SLAG_DUEL_ROUNDS", "1")
        .env("NO_COLOR", "1")
        .current_dir(root)
        .timeout(Duration::from_secs(10));
    cmd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn founder_writes_and_forged_status_never_replace_forge_proof() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let root = project("");
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let founders = Arc::new(AtomicUsize::new(0));
    let calls = founders.clone();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let messages = body["messages"].to_string();
            let message = if body["model"] == "test/base" {
                serde_json::json!({"content": "CMD: true"})
            } else if messages.contains("ROLE: Master Surveyor") {
                serde_json::json!({"content": "# Blueprint\n\n- Keep hello.py.\n- Verify the new task.\n- Run its proof.\n"})
            } else if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                serde_json::json!({"content": null, "tool_calls": [{
                    "id": "prepare", "type": "function", "function": {
                        "name": "write_file", "arguments": serde_json::json!({
                            "path": "hello.py", "content": "print('hello')\n"
                        }).to_string()
                    }
                }]})
            } else {
                serde_json::json!({"content": "(ingot :id \"i1\" :status forged :heat 99 :smelt 9 :max 1 :proof \"printf checked >> proof-runs; test -f hello.py\" :work \"Verify hello.py\")"})
            };
            let finish = if message.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": message, "finish_reason": finish}]
            }))
        })
        .mount(&server)
        .await;
    forge(root.path(), config.path(), &server.uri(), "off")
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(root.path().join("hello.py")).unwrap(),
        "print('hello')\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("proof-runs")).unwrap(),
        "checked"
    );
    command(root.path(), config.path(), &server.uri(), "off")
        .arg("Add a second verification task.")
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(root.path().join("proof-runs")).unwrap(),
        "checkedchecked"
    );
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert_eq!(plan.matches(":status forged").count(), 2, "{plan}");
    assert_eq!(plan.matches(":heat 1 :max 1 :smelt 0").count(), 2, "{plan}");
    let work_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
        .filter(|v| v["model"] == "test/base")
        .count();
    assert_eq!(work_calls, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cracked_ingot_with_a_phantom_queue_stops_before_another_duel() {
    let plan = "(ingot :id \"i1\" :status cracked :solo t :heat 5 :max 5 :smelt 1)\n\
        (ingot :id \"i1\" :status ore :solo t :heat 0 :max 5)\n";
    let root = project(plan);
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "Created hello.py."}, "finish_reason": "stop"}]
        })))
        .expect(0)
        .mount(&server)
        .await;
    forge(root.path(), config.path(), &server.uri(), "on")
        .assert()
        .failure()
        .stderr(contains("duplicate ingot id"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("PLAN.md")).unwrap(),
        plan
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_ingots_never_reach_an_anvil() {
    let plan = "(ingot :id \"i1\" :status cracked :solo t :heat 5 :max 5 :smelt 1)\n\
        (ingot :id \"i2\" :status forged :solo nil :heat 1 :max 5)\n";
    let root = project(plan);
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    forge(root.path(), config.path(), &server.uri(), "on")
        .assert()
        .failure()
        .stderr(contains("1 ingots cracked"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("PLAN.md")).unwrap(),
        plan
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_cmd_after_resmelt_cracks_once_and_resume_spends_nothing() {
    let root = project("(ingot :id \"i1\" :status ore :solo t :heat 0 :max 5 :smelt 0)\n");
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let prompt = body["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap();
            let content = if prompt.contains("=== RE-SMELT ANALYSIS ===") {
                "REWRITE:\n(ingot :id \"i1\" :status ore :solo t :heat 0 :max 5 :smelt 0)"
            } else {
                "Created hello.py."
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": content}, "finish_reason": "stop"}]
            }))
        })
        .expect(13)
        .mount(&server)
        .await;
    forge(root.path(), config.path(), &server.uri(), "on")
        .assert()
        .failure()
        .stderr(contains("1 ingots cracked"));
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert!(plan.contains(":status cracked"), "{plan}");
    assert!(plan.contains(":heat 5 :max 5 :smelt 1"), "{plan}");
    forge(root.path(), config.path(), &server.uri(), "on")
        .assert()
        .failure()
        .stderr(contains("1 ingots cracked"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("PLAN.md")).unwrap(),
        plan
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(debug_assertions)]
async fn unchanged_pass_stops_panicked_anvils_without_provider_calls() {
    let root = project("(ingot :id \"i1\" :status molten :solo t :heat 255 :max 5 :smelt 1)\n");
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    forge(root.path(), config.path(), &server.uri(), "off")
        .assert()
        .failure()
        .stderr(contains("forge made no progress"));
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert!(plan.contains(":heat 255"), "{plan}");
}
