use std::time::Duration;

use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn command(project: &tempfile::TempDir, config: &tempfile::TempDir, server: &MockServer) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_slag"));
    command.env_clear()
        .env("SLAG_CONFIG_DIR", config.path())
        .env("SLAG_BASE_URL", server.uri())
        .env("OPENAI_API_KEY", "test-key")
        .env("SLAG_DUEL", "off")
        .env("NO_COLOR", "1")
        .current_dir(project.path())
        .timeout(Duration::from_secs(15));
    command
}

fn seed(project: &tempfile::TempDir, plan: bool) {
    std::fs::write(project.path().join("PRD.md"), "Print hello.\n").unwrap();
    std::fs::write(project.path().join("BLUEPRINT.md"), "Print hello.\n").unwrap();
    std::fs::write(project.path().join("BAR.md"), "# Bar\n- Print hello\n- Pass tests\n").unwrap();
    if plan {
        std::fs::write(project.path().join("PLAN.md"),
            "(ingot :id \"hello\" :status ore :solo nil :grade 1 :heat 0 :max 5 :work \"Print hello\" :proof \"true\")\n").unwrap();
    }
}

fn assert_single_failure(output: &std::process::Output, role: &str) {
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("provider error: invalid key").count(), 1, "{stderr}");
    assert!(stderr.contains(&format!("role {role}")), "{stderr}");
    assert!(stderr.contains("OPENAI_API_KEY"), "{stderr}");
    assert!(!stdout.contains("FORGED"), "{stdout}");
    assert!(!stdout.contains("CRACKED"), "{stdout}");
    assert!(!stdout.contains("RE-SMELTING"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn survey_founder_and_plan_warden_stop_after_one_auth_failure() {
    for role in ["surveyor", "founder", "warden"] {
        let project = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        seed(&project, role == "warden");
        if role == "surveyor" {
            std::fs::remove_file(project.path().join("BLUEPRINT.md")).unwrap();
        }
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "Wrong API Key"}
            })))
            .expect(1).mount(&server).await;
        let mut cmd = command(&project, &config, &server);
        cmd.args(["--provider", "openai"]);
        if role == "warden" {
            cmd.args(["--temper", "1"]);
        }
        let output = cmd.arg("resume").assert().failure().get_output().clone();
        assert_single_failure(&output, role);
        server.verify().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn artifact_warden_auth_failure_cannot_report_success() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    seed(&project, true);
    let server = MockServer::start().await;
    let plans = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"model": "plan-model"})))
        .respond_with(move |_: &wiremock::Request| {
            if plans.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{"message": {"content": "VERDICT: pass\nEVIDENCE: plan covers the bar"}, "finish_reason": "stop"}]
                }))
            } else {
                ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "Wrong API Key"}}))
            }
        })
        .expect(2).mount(&server).await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"model": "work-model"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "", "tool_calls": [{"id": "done", "type": "function", "function": {
                "name": "finish", "arguments": "{\"summary\":\"Done\",\"cmd\":\"true\"}"
            }}]}, "finish_reason": "tool_calls"}]
        })))
        .expect(1).mount(&server).await;
    let output = command(&project, &config, &server)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["--provider", "openai", "--model", "work-model", "--plan-model", "plan-model", "--temper", "1", "resume"])
        .assert().failure().get_output().clone();
    assert_single_failure(&output, "warden");
    server.verify().await;
}
