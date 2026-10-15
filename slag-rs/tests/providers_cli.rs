use std::time::Duration;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn slag(project: &TempDir, config: &TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slag"));
    cmd.env_clear()
        .env("SLAG_CONFIG_DIR", config.path())
        .env("NO_COLOR", "1")
        .current_dir(project.path())
        .timeout(Duration::from_secs(15));
    cmd
}

#[test]
fn provider_flag_overrides_env_and_file_before_or_after_key() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("config.toml"), "provider = \"groq\"\n").unwrap();
    for args in [["--provider", "ollama", "key"], ["key", "--provider", "ollama"]] {
        slag(&project, &config)
            .env("SLAG_PROVIDER", "openai")
            .args(args)
            .assert()
            .success()
            .stdout(contains("ollama"));
    }
    slag(&project, &config)
        .env("SLAG_PROVIDER", "lmstudio")
        .arg("key")
        .assert()
        .success()
        .stdout(contains("lmstudio"));
    slag(&project, &config)
        .arg("key")
        .assert()
        .success()
        .stdout(contains("groq"))
        .stdout(contains("GROQ_API_KEY"));
}

#[test]
fn named_keys_preserve_other_settings_and_hide_key_values() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("config.toml"),
        "model_base = \"my-model\"\nopenrouter_api_key = \"old-router-key\"\n",
    ).unwrap();
    let secret = "test-secret-never-print-this-key";
    let output = slag(&project, &config)
        .args(["key", "--provider", "cerebras", secret])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    let contents = std::fs::read_to_string(config.path().join("config.toml")).unwrap();
    assert!(contents.contains("cerebras_api_key"));
    assert!(contents.contains(secret));
    assert!(contents.contains("old-router-key"));
    assert!(contents.contains("my-model"));
    let output = slag(&project, &config)
        .args(["key", "--provider", "cerebras"])
        .assert()
        .success()
        .get_output()
        .clone();
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(status.contains("cerebras"));
    assert!(!status.contains(secret));
    assert!(!status.contains("test-se"));
}

#[test]
fn missing_remote_key_names_the_selected_provider() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    slag(&project, &config)
        .args(["--provider", "cerebras", "resume"])
        .assert()
        .failure()
        .stderr(contains("CEREBRAS_API_KEY"));
}

#[test]
fn local_provider_passes_the_key_gate_without_a_key() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let output = slag(&project, &config)
        .args(["--provider", "ollama", "resume"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("no PRD.md"), "{error}");
    assert!(!error.contains("API_KEY"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn non_router_cost_uses_session_spend_without_a_balance_request() {
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".slag")).unwrap();
    std::fs::write(project.path().join(".slag/session-costs.json"), "0.25").unwrap();
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    slag(&project, &config)
        .env("SLAG_BASE_URL", server.uri())
        .env("OPENAI_API_KEY", "test-key")
        .args(["--provider", "openai", "cost"])
        .assert()
        .success()
        .stdout(contains("session $0.25"))
        .stderr(contains("session spend only"));
    slag(&project, &config)
        .env("SLAG_BASE_URL", server.uri())
        .args(["--provider", "openai", "cost"])
        .assert()
        .success()
        .stdout(contains("session $0.25"))
        .stderr(contains("session spend only"));
    assert!(server.received_requests().await.unwrap().is_empty());
}
