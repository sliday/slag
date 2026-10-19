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
        .expect(25)
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
async fn missing_finish_cmd_is_repaired_before_a_heat_burns() {
    let root = project("(ingot :id \"i1\" :status ore :solo nil :heat 0 :max 3 :proof \"true\")\n");
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"tool_calls": [{"id": "first", "type": "function",
                "function": {"name": "finish", "arguments": "{\"summary\":\"Done\"}"}}]},
                "finish_reason": "tool_calls"}]
        })))
        .up_to_n_times(1).expect(1).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"tool_calls": [{"id": "second", "type": "function",
                "function": {"name": "finish", "arguments": "{\"summary\":\"Done\",\"cmd\":\"true\"}"}}]},
                "finish_reason": "tool_calls"}]
        })))
        .expect(1).mount(&server).await;
    forge(root.path(), config.path(), &server.uri(), "off").assert().success();
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 1 :max 3 :smelt 0"), "{plan}");
    let requests = server.received_requests().await.unwrap();
    let requests: Vec<_> = requests.iter().filter(|request| request.method == "POST").collect();
    assert_eq!(requests.len(), 2);
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let finish = first["tools"].as_array().unwrap().iter()
        .find(|tool| tool["function"]["name"] == "finish").unwrap();
    assert!(finish["function"]["parameters"]["required"].as_array().unwrap().contains(&serde_json::json!("cmd")));
    let repair: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!(repair["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().contains("non-empty cmd string"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(debug_assertions)]
async fn exhausted_heat_cracks_without_provider_calls() {
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
        .stderr(contains("forge failed: 1 ingots cracked"));
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert!(plan.contains(":heat 255"), "{plan}");
    assert!(plan.contains(":status cracked"), "{plan}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(unix)]
async fn mock_smith_skips_heap_discovery_with_rg_and_find() {
    use std::os::unix::fs::PermissionsExt;

    for fallback in [false, true] {
        let root = project("(ingot :id \"i1\" :status ore :solo nil :max 1 :proof \"true\" :work \"Inspect project files\")\n");
        let config = tempfile::tempdir().unwrap();
        std::fs::rename(root.path().join(".git"), config.path().join("git-metadata")).unwrap();
        std::fs::write(
            root.path().join(".git"),
            format!("gitdir: {}\n", config.path().join("git-metadata").display()),
        )
        .unwrap();
        for file in [
            "logs/heap.txt",
            "src/logs/app.txt",
            "nested/slag-anvil-i1/cast.txt",
            "source.txt",
        ] {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "heap needle\n").unwrap();
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(|request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let n = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|m| m["role"] == "tool")
                    .count();
                let calls = match n {
                    0 => vec![
                        ("glob", serde_json::json!({"pattern": "**/*"})),
                        ("grep", serde_json::json!({"pattern": "."})),
                    ],
                    2 => vec![
                        (
                            "glob",
                            serde_json::json!({"pattern": "*.txt", "path": "logs"}),
                        ),
                        (
                            "grep",
                            serde_json::json!({"pattern": "needle", "path": "logs"}),
                        ),
                        ("read_file", serde_json::json!({"path": "logs/heap.txt"})),
                    ],
                    _ => vec![],
                };
                let (message, finish) =
                    if calls.is_empty() {
                        (serde_json::json!({"content": "CMD: true"}), "stop")
                    } else {
                        let tools: Vec<_> = calls.into_iter().enumerate().map(|(i, (name, args))| {
                        serde_json::json!({"id": format!("tool-{n}-{i}"), "type": "function",
                            "function": {"name": name, "arguments": args.to_string()}})
                    }).collect();
                        (
                            serde_json::json!({"content": null, "tool_calls": tools}),
                            "tool_calls",
                        )
                    };
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{"message": message, "finish_reason": finish}]
                }))
            })
            .expect(3)
            .mount(&server)
            .await;
        let mut cmd = forge(root.path(), config.path(), &server.uri(), "off");
        if fallback {
            let bin = config.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            let shell = bin.join("sh");
            std::fs::write(&shell, "#!/bin/sh\nif [ \"$1\" = \"-lc\" ]; then\n  exec /bin/sh -c 'command() { return 1; }; eval \"$1\"' sh \"$2\"\nfi\nexec /bin/sh \"$@\"\n").unwrap();
            std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
            cmd.env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(bin)
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            );
        }
        cmd.assert().success();
        let requests = server.received_requests().await.unwrap();
        let last: serde_json::Value =
            serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        let results: Vec<_> = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap())
            .collect();
        assert_eq!(results.len(), 5);
        for output in &results[..2] {
            assert!(
                output.contains("source.txt"),
                "fallback={fallback}: {output}"
            );
            assert!(
                output.contains("src/logs/app.txt"),
                "fallback={fallback}: {output}"
            );
            assert!(
                !output.lines().any(|l| l.starts_with("logs/")),
                "fallback={fallback}: {output}"
            );
            assert!(
                !output.contains("slag-anvil-i1"),
                "fallback={fallback}: {output}"
            );
            assert!(
                !output.lines().any(|l| l == ".git"),
                "fallback={fallback}: {output}"
            );
        }
        assert!(results[2].contains("logs/heap.txt"), "{}", results[2]);
        assert!(results[3].contains("logs/heap.txt"), "{}", results[3]);
        assert!(results[4].contains("heap needle"), "{}", results[4]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broken_proofs_resmelt_once_without_spending_retry_heats() {
    for (proof, error) in [
        ("if then", "syntax error"),
        ("slag_missing_proof_command", "command not found"),
        ("./check-proof", "Permission denied"),
        ("exit 126", "126"),
        ("exit 127", "127"),
    ] {
        let root = project(&format!(
            "(ingot :id \"i1\" :solo nil :max 5 :proof \"{proof}\")\n"
        ));
        std::fs::write(root.path().join("check-proof"), "#!/bin/sh\ntrue\n").unwrap();
        let config = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        let rewrite = format!("(ingot :id \"i1\" :solo nil :max 5 :proof \"{proof}\")");
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let content = if body["model"] == "test/plan" {
                    &rewrite
                } else if proof == "exit 127" {
                    "CMD: exit 127"
                } else {
                    "CMD: true"
                };
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{"message": {"content": content}, "finish_reason": "stop"}]
                }))
            })
            .mount(&server)
            .await;
        let out = forge(root.path(), config.path(), &server.uri(), "off")
            .assert()
            .failure()
            .get_output()
            .clone();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path().ends_with("/chat/completions"))
                .count(),
            3,
            "{proof}"
        );
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(output.contains("broken proof"), "{output}");
        assert!(output.contains(proof), "{output}");
        assert!(output.contains(error), "{output}");
        let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
        assert!(plan.contains(":status cracked"), "{plan}");
        assert!(plan.contains(":heat 0 :max 5 :smelt 1"), "{plan}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resmelt_rewrites_a_broken_proof_and_forges_the_ingot() {
    let root = project("(ingot :id \"i1\" :solo t :max 5 :proof \"if then\")\n");
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let content = if body["model"] == "test/plan" {
                "(ingot :id \"i1\" :solo t :max 5 :proof \"true\")"
            } else {
                "CMD: true"
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": content}, "finish_reason": "stop"}]
            }))
        })
        .mount(&server)
        .await;
    forge(root.path(), config.path(), &server.uri(), "off")
        .assert()
        .success();
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/chat/completions"))
            .count(),
        3
    );
    let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
    assert!(plan.contains(":status forged"), "{plan}");
    assert!(plan.contains(":heat 1 :max 5 :smelt 1"), "{plan}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quoted_proof_runs_after_plan_load_and_save() {
    let root = project(
        r#"(ingot :id "i1" :solo nil :max 5 :proof "printf '%s\n' \"a.b|c\" | grep 'a\.b\|c' > proof-ran")"#,
    );
    let config = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "CMD: true"}, "finish_reason": "stop"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    forge(root.path(), config.path(), &server.uri(), "off")
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(root.path().join("proof-ran")).unwrap(),
        "a.b|c\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_proof_failures_keep_retry_heats_and_show_the_error() {
    for (proof, cmd, error) in [
        (
            "echo invariant_failed >&2; false",
            "true",
            "invariant_failed",
        ),
        ("echo invalid_regex >&2; exit 2", "true", "invalid_regex"),
        ("true", "if then", "syntax error"),
    ] {
        let root = project(&format!(
            "(ingot :id \"i1\" :solo nil :max 3 :smelt 1 :proof \"{proof}\")\n"
        ));
        let config = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": format!("CMD: {cmd}")}, "finish_reason": "stop"}]
        })))
        .expect(3)
        .mount(&server)
        .await;
        let out = forge(root.path(), config.path(), &server.uri(), "off")
            .assert()
            .failure()
            .get_output()
            .clone();
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(output.contains(error), "{output}");
        assert!(!output.contains("broken proof"), "{output}");
        let plan = std::fs::read_to_string(root.path().join("PLAN.md")).unwrap();
        assert!(plan.contains(":heat 3 :max 3 :smelt 1"), "{plan}");
    }
}
