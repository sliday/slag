//! `slag fix --evolve` end to end against a mock model that answers by
//! role: each session's system prompt says whether it is a smith, an
//! editor, a composer, a selector, or the reflector, and the count of
//! assistant turns so far says which step of its script comes next. No
//! network and no real key.
//!
//! The scripted model has one behaviour a harness can change: a smith
//! whose system prompt carries `ALWAYS ADD A TEST` writes a regression
//! test beside the fix, and one without it does not. That is the
//! behavioural contrast the probes measure and the library records.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const RULE: &str = "ALWAYS ADD A TEST: write tests/value_test.sh beside every fix.";

/// Answers by role; keeps every request's system prompt for the asserts.
#[derive(Clone, Default)]
struct Roles {
    systems: Arc<Mutex<Vec<String>>>,
}

fn tool_call(calls: Vec<(&str, Value)>) -> ResponseTemplate {
    let calls: Vec<Value> = calls
        .into_iter()
        .enumerate()
        .map(|(i, (name, args))| {
            json!({"id": format!("c{i}"), "type": "function", "function": {"name": name, "arguments": args.to_string()}})
        })
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": "", "tool_calls": calls}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
    }))
}

fn text(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
    }))
}

const REFLECTION: &str = "<edits>\ncomponent: systemprompt.md\ncandidates: inv-1, comp-1\n\
    change: a rule to add a regression test beside every fix\n\
    observed: base-1 changed value.txt only; comp-1-run also wrote tests/value_test.sh\n\
    verdict: helped\n</edits>\n<experience>\ncue: the smith fixes the value but adds no regression test\n\
    lesson: put a one-line rule in systemprompt.md that names the test file to write beside the fix\n\
    evidence: row 1; base-1 vs comp-1-run in contrast.md\nconfidence: medium, one run each\n</experience>";

impl Respond for Roles {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let first = messages.first().and_then(|m| m["content"].as_str()).unwrap_or_default().to_string();
        let task = messages.get(1).and_then(|m| m["content"].as_str()).unwrap_or_default().to_string();
        let turn = messages.iter().filter(|m| m["role"] == "assistant").count();
        self.systems.lock().unwrap().push(first.clone());

        if first.contains("ROLE: reviewer of a bug fix") {
            return text("VERDICT: pass\nGAP:");
        }
        if first.contains("YOUR DIMENSION: INV") {
            return match turn {
                0 => tool_call(vec![("write_file", json!({"path": "context/harness/systemprompt.md", "content": format!("{RULE}\n")}))]),
                _ => text("<notes>\nsystemprompt.md: base-1 changed value.txt and wrote no test.\n</notes>"),
            };
        }
        if first.contains("YOUR DIMENSION: DOM") {
            // A recipe with no trigger, twice: the stage must fall back.
            let bad = json!({"path": "context/harness/recipes/values/RECIPE.md", "content": "---\nname: values\n---\nCheck values.\n"});
            let retry = task.contains("FAILED VALIDATION");
            return match (retry, turn) {
                (false, 0) => tool_call(vec![("write_file", bad)]),
                (true, 0) => tool_call(vec![("read_file", json!({"path": "context/harness/recipes/values/RECIPE.md"}))]),
                (true, 1) => tool_call(vec![("write_file", bad)]),
                _ => text("<notes>\nrecipes/values: a values recipe.\n</notes>"),
            };
        }
        if first.contains("You are composing") {
            return match turn {
                0 => tool_call(vec![(
                    "write_file",
                    json!({"path": "context/harness/systemprompt.md", "content": format!("{RULE}\nKeep the change small.\n")}),
                )]),
                _ => text("<notes>\nkept -> inv-1 systemprompt.md -> base-1 wrote no test; inv-1 did\n</notes>"),
            };
        }
        if first.contains("You are choosing which of") {
            return text("<why>\nB adds the test.\n</why>\n\n```choice\nB\n```");
        }
        if first.contains("You are reading the complete record") {
            return text(REFLECTION);
        }
        if first.contains("# slag smith") {
            if task.contains("REVIEW: ONE GAP LEFT") {
                return tool_call(vec![("finish", json!({"summary": "Nothing to change.", "cmd": "bash tests/check.sh"}))]);
            }
            let rule = first.contains("ALWAYS ADD A TEST");
            return match turn {
                0 => tool_call(vec![("read_file", json!({"path": "value.txt"}))]),
                1 if rule => tool_call(vec![
                    ("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"})),
                    ("write_file", json!({"path": "tests/value_test.sh", "content": "grep -qx 2 value.txt\n"})),
                ]),
                1 => tool_call(vec![("edit_file", json!({"path": "value.txt", "old_string": "1", "new_string": "2"}))]),
                _ => tool_call(vec![("finish", json!({"summary": "value.txt:1 held 1; it now holds 2.", "cmd": "bash tests/check.sh"}))]),
            };
        }
        text("unexpected request")
    }
}

async fn model() -> (MockServer, Roles) {
    let roles = Roles::default();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(roles.clone())
        .mount(&server)
        .await;
    (server, roles)
}

fn sh(dir: &Path, cmd: &str) -> String {
    let out = std::process::Command::new("bash").args(["-c", cmd]).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "{cmd}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `value.txt` says 1; the visible test wants 2.
fn buggy_repo() -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    sh(repo.path(), "git init -q -b main && git config user.email t@t && git config user.name t");
    std::fs::write(repo.path().join("value.txt"), "1\n").unwrap();
    std::fs::create_dir_all(repo.path().join("tests")).unwrap();
    std::fs::write(repo.path().join("tests/check.sh"), "grep -qx 2 value.txt\n").unwrap();
    sh(repo.path(), "git add -A && git commit -qm init");
    repo
}

fn slag(repo: &Path, server: &MockServer, home: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slag"));
    for var in ["SLAG_MODEL_PLAN", "SLAG_MODEL_ALT", "SLAG_MODEL_JUDGE", "SLAG_MODEL_EDITOR", "SLAG_DUEL", "SLAG_REASONING_EFFORT", "SLAG_HARNESS_LIBRARY", "SLAG_EVOLVE_BUDGET"] {
        cmd.env_remove(var);
    }
    cmd.env("OPENROUTER_API_KEY", "sk-or-test")
        .env("SLAG_OPENROUTER_BASE", server.uri())
        .env("SLAG_MODEL_BASE", "test/base")
        .env("SLAG_CONFIG_DIR", home)
        .env("SLAG_SESSIONS_DIR", home.join("sessions"))
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .current_dir(repo)
        .args(args)
        .timeout(Duration::from_secs(120));
    cmd
}

async fn run(cmd: Command) -> std::process::Output {
    let mut cmd = cmd;
    tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap()
}

fn record(repo: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(repo.join(".slag/evolve/latest.json")).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_evolved_fix_runs_every_stage_and_teaches_the_library() {
    let repo = buggy_repo();
    let home = tempfile::tempdir().unwrap();
    let (server, roles) = model().await;
    let args = ["fix", "value is wrong", "--test", "bash tests/check.sh", "--evolve", "1"];
    let out = run(slag(repo.path(), &server, home.path(), &args)).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("FIXED"), "{stdout}");
    for stage in ["reference", "propose", "probe", "compose", "execute", "reflect", "total"] {
        assert!(stdout.contains(stage), "the done screen shows the {stage} stage: {stdout}");
    }

    let rec = record(repo.path());
    let stages: Vec<&str> = rec["stages"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(stages, ["reference", "propose", "probe", "compose", "execute", "reflect"], "{rec:#}");
    for s in rec["stages"].as_array().unwrap() {
        assert!(s["total_tokens"].as_u64().unwrap() > 0, "every stage spent tokens: {s}");
        assert!(s["secs"].as_f64().is_some() && s["spend"].as_str().is_some(), "{s}");
    }
    let cand = |label: &str| rec["candidates"].as_array().unwrap().iter().find(|c| c["label"] == label).unwrap().clone();
    assert_eq!(cand("inv-1")["outcome"], "edited");
    assert_eq!(cand("dom-1")["outcome"], "fallback", "an invalid tree twice falls back: {rec:#}");
    assert_eq!(cand("dom-1")["retried"], true);
    assert!(cand("dom-1")["fallback"].as_str().unwrap().contains("has no trigger"));
    assert_eq!(cand("comp-1")["outcome"], "edited");

    // The probe of a fallen-back candidate reuses the reference run; the
    // composed harness changed behaviour: its run wrote a regression test.
    let run_of = |label: &str| rec["runs"].as_array().unwrap().iter().find(|r| r["label"] == label).unwrap().clone();
    assert_eq!(run_of("dom-1")["reused"], "base-1");
    assert_eq!(run_of("base-1")["files"], json!(["value.txt"]));
    let composed = run_of("comp-1-run");
    assert_eq!(composed["verified"], true);
    assert!(composed["files"].as_array().unwrap().contains(&json!("tests/value_test.sh")), "{composed}");
    assert_eq!(rec["selection"]["by"], "composed");
    assert_eq!(rec["adopted"], "comp-1");

    // The delivered fix carries the composed run's test, and slag's own
    // reversal check proves it.
    let files = sh(repo.path(), "git show --name-only --format= HEAD");
    assert!(files.contains("tests/value_test.sh") && files.contains("value.txt"), "{files}");
    assert!(stdout.contains("without the fix"), "{stdout}");

    // Harnessing-level recursion: one contrast-backed entry, indexed.
    assert!(rec["library"].as_str().unwrap().starts_with("added"), "{rec:#}");
    let domain = home.path().join("library/shell");
    let index = std::fs::read_to_string(domain.join("INDEX.md")).unwrap();
    assert!(index.contains("value-is-wrong") && index.contains("systemprompt.md"), "{index}");
    let entry = std::fs::read_dir(&domain).unwrap().flatten().find(|e| e.path().is_dir()).unwrap().path();
    let exp = std::fs::read_to_string(entry.join("EXPERIENCE.md")).unwrap();
    assert!(exp.contains("cue: the smith fixes the value") && exp.contains("verdict: helped"), "{exp}");
    assert!(std::fs::read_to_string(entry.join("harness/systemprompt.md")).unwrap().contains(RULE));

    // Improver sessions never saw a shell or a finish tool.
    let systems = roles.systems.lock().unwrap().clone();
    assert!(systems.iter().any(|s| s.contains("YOUR DIMENSION: INV")));

    // `slag status --json` carries the stage table.
    let status = run(slag(repo.path(), &server, home.path(), &["status", "--json"])).await;
    let json: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["evolve"]["adopted"], "comp-1", "{json:#}");
    assert_eq!(json["evolve"]["stages"].as_array().unwrap().len(), 6);
    assert!(json["evolve"]["total"]["total_tokens"].as_u64().unwrap() > 0);

    // The evidence an improver read holds no absolute path to the repo.
    let task = std::fs::read_to_string(repo.path().join(format!(".slag/evolve/{}/sessions/inv-1/context/task.md", rec["run"].as_str().unwrap()))).unwrap();
    assert!(!task.contains(&repo.path().display().to_string()), "{task}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_library_path_applies_the_lesson_with_no_probing() {
    let home = tempfile::tempdir().unwrap();
    let (server, roles) = model().await;
    let first = buggy_repo();
    let args = ["fix", "value is wrong", "--test", "bash tests/check.sh", "--evolve", "1"];
    let out = run(slag(first.path(), &server, home.path(), &args)).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));

    // A second repository of the same kind, on the cheap path.
    let second = buggy_repo();
    roles.systems.lock().unwrap().clear();
    let args = ["fix", "value is off", "--test", "bash tests/check.sh", "--harness", "library"];
    let out = run(slag(second.path(), &server, home.path(), &args)).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("harness: shell library entries"), "{stdout}");
    let systems = roles.systems.lock().unwrap().clone();
    assert!(!systems.iter().any(|s| s.contains("You are composing") || s.contains("YOUR DIMENSION")), "no improver ran");
    let smith = systems.iter().find(|s| s.contains("# slag smith")).expect("a smith ran");
    assert!(smith.contains("# Harness") && smith.contains(RULE), "the lesson reached the smith's prompt");
    let files = sh(second.path(), "git show --name-only --format= HEAD");
    assert!(files.contains("tests/value_test.sh"), "the applied harness changed behaviour: {files}");

    // Without --harness, the fast path's prompt carries no harness at all.
    let third = buggy_repo();
    roles.systems.lock().unwrap().clear();
    let args = ["fix", "value is off", "--test", "bash tests/check.sh"];
    let out = run(slag(third.path(), &server, home.path(), &args)).await;
    assert!(out.status.success());
    let systems = roles.systems.lock().unwrap().clone();
    let smith = systems.iter().find(|s| s.contains("# slag smith")).unwrap();
    assert!(!smith.contains("# Harness"), "{smith}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cap_stops_the_recursion_and_keeps_the_verified_fix() {
    let repo = buggy_repo();
    let home = tempfile::tempdir().unwrap();
    let (server, roles) = model().await;
    let args = ["fix", "value is wrong", "--test", "bash tests/check.sh", "--evolve", "1", "--evolve-tokens", "1"];
    let out = run(slag(repo.path(), &server, home.path(), &args)).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("FIXED") && stdout.contains("capped"), "{stdout}");
    let rec = record(repo.path());
    assert!(rec["capped"].as_str().unwrap().contains("token cap"), "{rec:#}");
    let stages: Vec<&str> = rec["stages"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(stages, ["reference"], "nothing past the cap runs");
    assert_eq!(rec["selection"]["chosen"], "base-1");
    assert!(rec["library"].as_str().unwrap().contains("reflection skipped"));
    let systems = roles.systems.lock().unwrap().clone();
    assert!(!systems.iter().any(|s| s.contains("YOUR DIMENSION")), "no editor ran past the cap");
    assert_eq!(sh(repo.path(), "cat value.txt").trim(), "2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_wide_evolution_picks_blind_among_verified_fixes() {
    let repo = buggy_repo();
    let home = tempfile::tempdir().unwrap();
    let (server, roles) = model().await;
    let args = ["fix", "value is wrong", "--test", "bash tests/check.sh", "--evolve", "2"];
    let out = run(slag(repo.path(), &server, home.path(), &args)).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    let rec = record(repo.path());
    let labels: Vec<&str> = rec["candidates"].as_array().unwrap().iter().map(|c| c["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["inv-1", "dom-1", "inv-2", "dom-2", "comp-1", "comp-2"]);
    let selection = &rec["selection"];
    assert_eq!(selection["by"], "selector", "{rec:#}");
    // The selector saw anonymous ids only; B maps back to a real run.
    let chosen = selection["chosen"].as_str().unwrap();
    assert_eq!(selection["ids"][chosen], "B");
    let systems = roles.systems.lock().unwrap().clone();
    let picks = systems.iter().filter(|s| s.contains("You are choosing which of")).count();
    assert_eq!(picks, 1);
    let candidates = std::fs::read_dir(repo.path().join(format!(".slag/evolve/{}/sessions/select/context/candidates", rec["run"].as_str().unwrap())))
        .unwrap()
        .count();
    assert!(candidates >= 2, "the pool holds the distinct verified fixes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequential_evolution_chains_checkpoints() {
    let repo = buggy_repo();
    let home = tempfile::tempdir().unwrap();
    let (server, _) = model().await;
    let args = ["fix", "value is wrong", "--test", "bash tests/check.sh", "--evolve", "2", "--sequential"];
    let out = run(slag(repo.path(), &server, home.path(), &args)).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    let rec = record(repo.path());
    assert_eq!(rec["mode"], "sequential");
    let labels: Vec<&str> = rec["candidates"].as_array().unwrap().iter().map(|c| c["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["inv-r1", "dom-r1", "ckpt-1", "inv-r2", "dom-r2", "ckpt-2"]);
    let stages: Vec<&str> = rec["stages"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert!(stages.contains(&"compose-2") && stages.contains(&"execute-2"), "{stages:?}");
    // Round 2's editors read round 1 as history.
    let history = repo.path().join(format!(".slag/evolve/{}/sessions/inv-r2/context/history/round-1/ckpt-1/diff.md", rec["run"].as_str().unwrap()));
    assert!(history.exists(), "{}", history.display());
}

#[test]
fn sequential_without_evolve_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_slag"))
        .env("OPENROUTER_API_KEY", "sk-or-test")
        .env("SLAG_CONFIG_DIR", dir.path())
        .current_dir(dir.path())
        .args(["fix", "x", "--sequential"])
        .timeout(Duration::from_secs(30))
        .assert()
        .failure()
        .stderr(predicates::str::contains("--sequential needs --evolve"));
}
