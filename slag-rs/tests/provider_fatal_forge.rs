use std::time::Duration;

use assert_cmd::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread")]
async fn forge_auth_failure_preserves_heats_and_never_resmelts() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("PRD.md"), "Write hello.py.\n").unwrap();
    std::fs::write(project.path().join("BLUEPRINT.md"), "Write hello.py.\n").unwrap();
    for solo in ["nil", "t"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(200)).set_body_json(serde_json::json!({
                "error": {"message": "Wrong API Key"}
            })))
            .expect(if solo == "t" { 1..=2 } else { 1..=1 })
            .mount(&server)
            .await;
        let ids = if solo == "t" { vec!["hello", "second"] } else { vec!["hello"] };
        let plan: String = ids.iter().map(|id| format!(
            "(ingot :id \"{id}\" :status ore :solo {solo} :grade 1 :heat 0 :max 5 :work \"Write hello.py\" :proof \"true\")\n"
        )).collect();
        std::fs::write(project.path().join("PLAN.md"), plan).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_slag"))
            .env_clear()
            .env("SLAG_CONFIG_DIR", config.path())
            .env("SLAG_BASE_URL", server.uri())
            .env("OPENAI_API_KEY", "bad-test-key")
            .env("SLAG_DUEL", "off")
            .env("NO_COLOR", "1")
            .current_dir(project.path())
            .timeout(Duration::from_secs(15))
            .args(["--provider", "openai", "resume"])
            .assert()
            .failure()
            .get_output().clone();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stderr.contains("401"), "{stderr}");
        assert!(!stdout.contains("FORGED"), "{stdout}");
        assert!(!stdout.contains("CRACKED"), "{stdout}");
        assert!(!stdout.contains("single-smith fallback"), "{stdout}");
        let plan = std::fs::read_to_string(project.path().join("PLAN.md")).unwrap();
        assert_eq!(plan.matches(":heat 0").count(), ids.len(), "{plan}");
        assert_eq!(plan.matches(":status ore").count(), ids.len(), "{plan}");
        assert!(!plan.contains(":smelt 1"), "{plan}");
        let entries = std::fs::read_dir(project.path().join("logs")).unwrap();
        assert!(!entries.flatten().any(|entry| entry.file_name().to_string_lossy().contains("RESMELT")));
        server.verify().await;
    }
}
