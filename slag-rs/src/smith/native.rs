//! NativeSmith — engine-backed Smith. The internal agent loop replaces the
//! `claude -p` subprocess: same `Smith` boundary, zero external CLIs.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use super::{EngineHooks, Smith};
use crate::config::EngineConfig;
use crate::engine::agent::{ForgeAgent, SpendAccum};
use crate::engine::Role;
use crate::engine::events::{self, StderrNarrator};
use crate::engine::prompt::{self, PromptMode};
use crate::engine::provider::OpenRouter;
use crate::engine::tools::ToolBox;
use crate::engine::{Effort, EventTx, Provider};
use crate::error::SlagError;

/// Engine-backed smith. One invoke = one full agent session.
pub struct NativeSmith {
    cfg: EngineConfig,
    grade: u8,
    /// Reserved for recipe selection (v2 skills system).
    #[allow(dead_code)]
    skill: String,
    root: PathBuf,
    mode: PromptMode,
    /// Explicit model pin for duel casts; None follows grade selection.
    model_override: Option<String>,
    hooks: EngineHooks,
    /// One accumulator per smith: every invoke (heat, transient retry) of
    /// this smith shares it, so `SLAG_MAX_COST_INGOT` caps the whole
    /// ingot's spend rather than each session starting from $0.
    ingot_spend: SpendAccum,
    /// Ledger attribution for this smith's calls (item 35). Forge passes
    /// are `Smith`; the plan smith carries whichever phase built it, so
    /// surveyor and founder spend separate even on one model.
    role: Role,
    scope_only: bool,
    /// The execution harness (`crate::harness`) this smith runs under.
    /// `None` builds the prompt, tools and hooks exactly as before.
    harness: Option<Arc<crate::harness::Harness>>,
    /// Where the session transcript goes; `None` keeps the run directory.
    transcript_root: Option<PathBuf>,
    /// A dollar cap over `ingot_spend` that overrides `SLAG_MAX_COST_INGOT`.
    cost_cap: Option<f64>,
}

impl NativeSmith {
    /// Full tool-use forge pass. Mirrors `ClaudeSmith::from_config`.
    pub fn forge(cfg: EngineConfig, skill: &str, grade: u8, root: PathBuf, hooks: &EngineHooks) -> Self {
        Self {
            cfg,
            grade,
            skill: skill.to_string(),
            root,
            mode: PromptMode::Forge,
            model_override: None,
            hooks: hooks.clone(),
            ingot_spend: SpendAccum::default(),
            role: Role::Smith,
            scope_only: false,
            harness: None,
            transcript_root: None,
            cost_cap: None,
        }
    }

    /// Attribute this smith's spend to a different ledger row.
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    /// Survey/plan pass on the reasoning model. Mirrors `ClaudeSmith::plan`.
    pub fn plan(cfg: EngineConfig, root: PathBuf, hooks: &EngineHooks) -> Self {
        Self {
            cfg,
            grade: crate::config::HIGH_GRADE,
            skill: String::new(),
            root,
            mode: PromptMode::Plan,
            model_override: None,
            hooks: hooks.clone(),
            ingot_spend: SpendAccum::default(),
            role: Role::Plan,
            scope_only: false,
            harness: None,
            transcript_root: None,
            cost_cap: None,
        }
    }

    pub fn scope(cfg: EngineConfig, root: PathBuf, hooks: &EngineHooks) -> Self {
        Self {
            grade: 1,
            role: Role::Surveyor,
            scope_only: true,
            ..Self::plan(cfg, root, hooks)
        }
    }

    pub fn review(cfg: EngineConfig, root: PathBuf, hooks: &EngineHooks) -> Self {
        Self {
            mode: PromptMode::Review,
            role: Role::Warden,
            ..Self::plan(cfg, root, hooks)
        }
    }

    /// Duel cast: a forge pass pinned to an explicit model (base or alt),
    /// rooted at the cast's worktree. Casts never take steer/cancel from
    /// the dashboard directly — the duel loop owns their lifecycle — but
    /// their events still fan out through `hooks.events`.
    pub fn cast(
        cfg: EngineConfig,
        skill: &str,
        grade: u8,
        root: PathBuf,
        model: &str,
        hooks: &EngineHooks,
    ) -> Self {
        Self {
            cfg,
            grade,
            skill: skill.to_string(),
            root,
            mode: PromptMode::Forge,
            model_override: Some(model.to_string()),
            hooks: EngineHooks {
                events: hooks.events.clone(),
                steer: None,
                cancel: hooks.cancel.clone(),
            },
            ingot_spend: SpendAccum::default(),
            role: Role::Duel,
            scope_only: false,
            harness: None,
            transcript_root: None,
            cost_cap: None,
        }
    }

    /// Share a spend accumulator with other smiths of the same ingot
    /// (duel casts are rebuilt every round; the accumulator persists).
    pub fn with_ingot_spend(mut self, acc: SpendAccum) -> Self {
        self.ingot_spend = acc;
        self
    }

    /// Run under an execution harness. An empty one changes nothing.
    pub fn with_harness(mut self, harness: Option<Arc<crate::harness::Harness>>) -> Self {
        self.harness = harness.filter(|h| !h.is_empty());
        self
    }

    /// Journal the session's transcript under `root` instead of the run
    /// directory.
    pub fn with_transcript_root(mut self, root: Option<PathBuf>) -> Self {
        self.transcript_root = root;
        self
    }

    /// Cap the shared spend accumulator at `cap` dollars.
    pub fn with_cost_cap(mut self, cap: Option<f64>) -> Self {
        self.cost_cap = cap;
        self
    }

    fn model(&self) -> &str {
        if let Some(model) = &self.model_override {
            return model;
        }
        match self.mode {
            PromptMode::Plan | PromptMode::Review => &self.cfg.model_plan,
            PromptMode::Forge => self.cfg.model_for_grade(self.grade),
        }
    }

    /// Build the provider with its observability wired: the event sink
    /// carries `ApiRetry` heartbeats through unattended waits (without it
    /// a rate-limit wait looks like a frozen run), and the cancel flag
    /// lets Ctrl-C abort a retry wait instead of sleeping out the window.
    fn build_provider(&self, tx: &EventTx) -> Arc<OpenRouter> {
        let provider = Arc::new(OpenRouter::with_base_url(
            self.cfg.api_key.clone(),
            self.cfg.base_url.clone(),
        ));
        provider.set_event_sink(tx.clone());
        if let Some(cancel) = &self.hooks.cancel {
            provider.set_cancel_flag(cancel.clone());
        }
        provider
    }

    async fn invoke_impl(&self, prompt_text: &str) -> Result<String, SlagError> {
        if let Some(error) = crate::error::provider_run_failure() {
            return Err(error);
        }
        let model = self.model().to_string();
        let mut system = if self.scope_only {
            "Classify the commission's scope. Return only the requested JSON. Do not build or inspect files. For a fresh Python unittest project, choose python3 -m unittest so the proof discovers the complete test suite.".into()
        } else {
            prompt::build_with(&self.root, &model, self.mode, self.harness.as_deref()).join()
        };
        if self.role == Role::Warden {
            system.push_str("\n\nREAD-ONLY REVIEW: You cannot write anywhere, including temporary directories. \
                Read the relevant source, tests, or document first. \
                Use read-only-compatible checks, existing fixtures, or in-memory test inputs. \
                Disable optional test caches. Python bytecode and Node compile caches are disabled by the harness. \
                Return unverified if a required check needs writes or unavailable dependencies. \
                A denied write ends this review as unverified. Return your verdict as text after inspection. \
                EVIDENCE must be a JSON array of successful tool-call IDs from this session, for example [\"read-source\"]. \
                Each successful tool result names its inspection ID. Do not put paths, commands, or prose in EVIDENCE. \
                The coordinator resolves those IDs to the actual successful reads and commands.");
        }
        if self.mode == PromptMode::Review {
            system.push_str("\nRun the declared test suite and requested CLI through bash before an artifact pass. \
                A source read or old proof log cannot replace your own successful test and CLI runs. \
                Cite the successful run IDs in EVIDENCE.");
        }
        let effort = if self.scope_only {
            Some(Effort::Low)
        } else {
            self.cfg.effort.or(Some(if self.mode == PromptMode::Review {
                Effort::Low
            } else {
                Effort::from_grade(self.grade)
            }))
        };

        // One agent channel fans out to the JSONL sink, the display, and
        // (when the TUI is up) the dashboard hook. The dashboard replaces
        // the stderr narrator: both writing to the terminal at once would
        // corrupt the alternate screen.
        let (tx, mut rx) = events::channel();
        let (tx_jsonl, rx_jsonl) = events::channel();
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%f");
        let jsonl_path = PathBuf::from(crate::config::LOG_DIR).join(format!("engine-{ts}.jsonl"));
        let jsonl = events::spawn_jsonl_sink(rx_jsonl, jsonl_path);

        let hook_tx = self.hooks.events.clone();
        let (narrator_tx, narrator) = if hook_tx.is_none() {
            let (tx_narr, rx_narr) = events::channel();
            (Some(tx_narr), Some(StderrNarrator::spawn_narrator(rx_narr)))
        } else {
            (None, None)
        };
        let fanout = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let _ = tx_jsonl.send(event.clone());
                if let Some(hook) = &hook_tx {
                    let _ = hook.send(event.clone());
                }
                if let Some(narr) = &narrator_tx {
                    if crate::error::provider_run_failure().is_none()
                        || !matches!(&event, crate::engine::EngineEvent::Error { .. })
                    {
                        let _ = narr.send(event);
                    }
                }
            }
        });

        let provider = self.build_provider(&tx);
        // Size the compaction budget to the model's real window (cached
        // per model on the provider; `None` on fetch failure keeps the
        // default): a 32k model compacts before it 400s, a 1M model does
        // not throw context away at the fixed default.
        let window = if self.scope_only { None } else { provider.context_length(&model).await };
        let toolbox = ToolBox::new(&self.root)
            .with_policy(crate::engine::policy::Policy::from_config())
            .with_steer(self.hooks.steer.clone())
            .with_harness_recipes(self.harness.as_ref().and_then(|h| h.recipe_root()));
        let mut agent = ForgeAgent::new(provider, toolbox, &model)
            .with_coordinator_root(self.root.clone())
            .with_context_window(window)
            .with_effort(effort)
            .with_ingot_spend(self.ingot_spend.clone())
            .with_role(self.role)
            .with_events(tx);
        // A survey pass delivers a document, and the loop returns the
        // model's text verbatim only when no tool was called. Left with a
        // `finish` tool the model reads, finishes, and never writes the
        // document -- so the tool is withheld rather than argued with.
        if self.mode == PromptMode::Plan {
            agent = agent.without_finish();
        }
        if self.mode == PromptMode::Forge {
            agent = agent.with_required_verification();
            if self.grade == 1 && prompt_text.lines().any(|line| line.starts_with("REQUIRED RUN COMMANDS: ")) {
                agent = agent.with_max_turns(16);
            }
        }
        if self.role == Role::Warden {
            agent = agent.with_read_only_tools().with_max_turns(2);
        }
        if self.mode == PromptMode::Review {
            agent = agent.with_read_only_tools().with_required_inspection().with_required_runs().with_max_turns(6);
        }
        if self.scope_only {
            agent = agent.with_no_tools().with_max_turns(1);
        }
        if let Some(steer) = &self.hooks.steer {
            agent = agent.with_steer(steer.clone());
        }
        if let Some(cancel) = &self.hooks.cancel {
            agent = agent.with_cancel(cancel.clone());
        }
        if let Some(harness) = &self.harness {
            agent = agent.with_harness_hooks(harness.hooks(&self.root).map(Arc::new));
        }
        if let Some(root) = &self.transcript_root {
            agent = agent.with_transcript_root(root.clone());
        }
        if self.cost_cap.is_some() {
            agent = agent.with_cost_cap(self.cost_cap);
        }

        let result = agent.run(system, prompt_text.to_string()).await;

        // Drop the agent (and its EventTx) so the sinks drain and exit.
        drop(agent);
        let _ = fanout.await;
        let _ = jsonl.await;
        if let Some(narrator) = narrator {
            let _ = narrator.await;
        }

        result
    }
}

impl Smith for NativeSmith {
    fn invoke(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move { self.invoke_impl(&prompt).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EngineConfig {
        EngineConfig {
            api_key: "sk-or-test".into(),
            model_base: "base/model".into(),
            model_plan: "plan/model".into(),
            model_alt: "alt/model".into(),
            model_judge: "judge/model".into(),
            effort: None,
            base_url: crate::engine::OPENROUTER_BASE.into(),
            duel: crate::config::DuelMode::Auto,
            duel_rounds_override: None,
            screenshot_cmd: None,
        }
    }

    #[test]
    fn an_empty_harness_is_no_harness() {
        let dir = tempfile::tempdir().unwrap();
        let smith = NativeSmith::forge(cfg(), "default", 1, dir.path().into(), &EngineHooks::default())
            .with_harness(Some(Arc::new(crate::harness::Harness::load(dir.path()))));
        assert!(smith.harness.is_none(), "an empty harness must not reach the prompt");
        std::fs::write(dir.path().join("memory.md"), "Tests use unittest.\n").unwrap();
        let smith = smith.with_harness(Some(Arc::new(crate::harness::Harness::load(dir.path()))));
        assert!(smith.harness.is_some());
    }

    #[test]
    fn forge_picks_model_by_grade() {
        let hooks = EngineHooks::default();
        let low = NativeSmith::forge(cfg(), "rust", 1, PathBuf::from("."), &hooks);
        assert_eq!(low.model(), "base/model");
        let high =
            NativeSmith::forge(cfg(), "rust", crate::config::HIGH_GRADE, PathBuf::from("."), &hooks);
        assert_eq!(high.model(), "plan/model");
    }

    #[test]
    fn plan_uses_plan_model_and_plan_mode() {
        let smith = NativeSmith::plan(cfg(), PathBuf::from("."), &EngineHooks::default());
        assert_eq!(smith.model(), "plan/model");
        assert_eq!(smith.mode, PromptMode::Plan);
    }

    #[tokio::test]
    async fn scope_uses_one_small_request_without_tools_or_a_window_lookup() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "{\"direct\":true,\"proof\":\"python3 -m unittest\",\"criteria\":[\"The local utility passes its requested unittest.\"],\"commands\":[]}"}, "finish_reason": "stop"}],
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = cfg();
        config.base_url = server.uri();
        config.effort = Some(Effort::High);
        let smith = NativeSmith::scope(config, PathBuf::from("."), &EngineHooks::default());
        let proof = crate::pipeline::surveyor::direct_proof(&smith, "Create a local utility with a unittest.")
            .await.unwrap();
        assert_eq!(proof.as_ref().map(|plan| plan.proof.as_str()), Some("python3 -m unittest"));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["model"], "plan/model");
        assert_eq!(body["reasoning"]["effort"], "low");
        assert!(body.get("tools").is_none_or(|tools| tools.as_array().unwrap().is_empty()));
        assert!(body["messages"][0]["content"].as_str().unwrap().len() < 300);
    }

    #[tokio::test]
    async fn warden_inspects_files_and_runs_checks_without_builder_tools() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("output.txt"), "accepted\n").unwrap();
        let read_only_available = crate::engine::sandbox::Sandbox::read_only().is_ok();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(|request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let inspected = body["messages"].as_array().unwrap().iter()
                    .any(|message| message["role"] == "tool");
                let message = if inspected {
                    serde_json::json!({
                        "content": "VERDICT: pass\nGAP:\nEVIDENCE: [\"source\", \"check\"]",
                    })
                } else {
                    serde_json::json!({
                        "content": "",
                        "tool_calls": [
                            {"id": "source", "type": "function", "function": {
                                "name": "read_file", "arguments": "{\"path\":\"output.txt\"}",
                            }},
                            {"id": "check", "type": "function", "function": {
                                "name": "bash", "arguments": "{\"command\":\"cat output.txt\"}",
                            }},
                        ],
                    })
                };
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{"message": message, "finish_reason": if inspected { "stop" } else { "tool_calls" }}],
                }))
            })
            .expect(if read_only_available { 2 } else { 1 })
            .mount(&server)
            .await;

        let mut config = cfg();
        config.base_url = server.uri();
        let smith = NativeSmith::review(config, dir.path().into(), &EngineHooks::default());
        let result = smith.invoke("Review output.txt.").await;
        if read_only_available {
            assert!(crate::pipeline::warden::parse_verdict(&result.unwrap()).fulfilled);
        } else {
            assert!(result.unwrap_err().to_string().contains("review policy violation:"));
        }
        let requests = server.received_requests().await.unwrap();
        let bodies: Vec<serde_json::Value> = requests.iter()
            .filter(|request| request.url.path() == "/chat/completions")
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        for body in &bodies {
            let tools: Vec<_> = body["tools"].as_array().unwrap().iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap())
                .collect();
            assert!(tools.contains(&"bash"));
            assert!(tools.contains(&"read_file"));
            assert!(!tools.iter().any(|name| ["write_file", "edit_file", "finish"].contains(name)));
        }
        assert_eq!(bodies.len(), if read_only_available { 2 } else { 1 });
        if read_only_available {
            let results: Vec<_> = bodies[1]["messages"].as_array().unwrap().iter()
                .filter(|message| message["role"] == "tool")
                .collect();
            assert_eq!(results.len(), 2);
            assert!(results.iter().all(|message| message["content"].as_str().unwrap().contains("accepted")));
        }
        assert_eq!(std::fs::read_to_string(dir.path().join("output.txt")).unwrap(), "accepted\n");
    }

    #[tokio::test]
    async fn artifact_warden_cannot_pass_with_only_source_reads() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("output.txt"), "accepted\n").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(|request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let inspected = body["messages"].as_array().unwrap().iter()
                    .any(|message| message["role"] == "tool");
                let message = if inspected {
                    serde_json::json!({"content": "VERDICT: pass\nEVIDENCE: [\"source\"]"})
                } else {
                    serde_json::json!({"tool_calls": [{"id": "source", "type": "function", "function": {
                        "name": "read_file", "arguments": "{\"path\":\"output.txt\"}",
                    }}]})
                };
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{"message": message, "finish_reason": if inspected { "stop" } else { "tool_calls" }}],
                }))
            }).expect(2).mount(&server).await;
        let mut config = cfg();
        config.base_url = server.uri();
        let smith = NativeSmith::review(config, dir.path().into(), &EngineHooks::default());
        let result = smith.invoke("Review output.txt.").await.unwrap();
        assert!(crate::pipeline::warden::parse_verdict(&result).is_unverified(), "{result}");
    }

    #[tokio::test]
    async fn warden_sessions_cannot_pass_after_a_forbidden_mutation() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for document in [true, false] {
            for shell in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                std::fs::write(dir.path().join("artifact.txt"), "original\n").unwrap();
                let server = MockServer::start().await;
                let mutation = if shell {
                    serde_json::json!({"name": "bash", "arguments": serde_json::json!({
                        "command": "python3 -c 'from pathlib import Path; Path(\"artifact.txt\").write_text(\"tampered\")'",
                    }).to_string()})
                } else {
                    serde_json::json!({"name": "write_file", "arguments": serde_json::json!({
                        "path": "artifact.txt", "content": "tampered",
                    }).to_string()})
                };
                Mock::given(method("POST"))
                    .and(path("/chat/completions"))
                    .respond_with(move |request: &wiremock::Request| {
                        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                        let inspected = body["messages"].as_array().unwrap().iter()
                            .any(|message| message["role"] == "tool");
                        let message = if inspected {
                            serde_json::json!({"content": "VERDICT: pass\nGAP:\nEVIDENCE: read artifact.txt"})
                        } else {
                            serde_json::json!({"content": null, "tool_calls": [
                                {"id": "inspect", "type": "function", "function": {
                                    "name": "read_file", "arguments": "{\"path\":\"artifact.txt\"}",
                                }},
                                {"id": "mutate", "type": "function", "function": mutation},
                            ]})
                        };
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({
                            "choices": [{"message": message, "finish_reason": if inspected { "stop" } else { "tool_calls" }}],
                        }))
                    })
                    .mount(&server).await;

                let mut config = cfg();
                config.base_url = server.uri();
                let hooks = EngineHooks::default();
                let smith = if document {
                    NativeSmith::plan(config, dir.path().into(), &hooks).with_role(Role::Warden)
                } else {
                    NativeSmith::review(config, dir.path().into(), &hooks)
                };
                let result = smith.invoke("Inspect artifact.txt and return a verdict.").await;
                assert!(result.is_err(), "document={document}, shell={shell}: {result:?}");
                assert_eq!(std::fs::read_to_string(dir.path().join("artifact.txt")).unwrap(), "original\n");
                let requests = server.received_requests().await.unwrap();
                let request = requests.iter().find(|request| request.url.path() == "/chat/completions").unwrap();
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let tools: Vec<_> = body["tools"].as_array().unwrap().iter()
                    .map(|tool| tool["function"]["name"].as_str().unwrap())
                    .collect();
                assert!(tools.contains(&"read_file"));
                assert!(tools.contains(&"bash"));
                assert!(!tools.iter().any(|name| ["write_file", "edit_file", "finish"].contains(name)));
            }
        }
    }

    #[tokio::test]
    async fn invoke_fetches_the_model_window_to_size_the_token_budget() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // `.expect(1)` is verified on MockServer drop: the /models window
        // fetch must run as part of a real invoke — regression for the
        // window-derived budget being dead code outside unit tests.
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "id": "base/model", "context_length": 32768 }],
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": { "content": "done\nCMD: true" },
                    "finish_reason": "stop",
                }],
            })))
            .mount(&server)
            .await;

        let mut config = cfg();
        config.base_url = server.uri();
        let smith =
            NativeSmith::forge(config, "rust", 1, PathBuf::from("."), &EngineHooks::default());
        assert_eq!(smith.invoke("task").await.unwrap(), "done\nCMD: true");
    }

    /// Regression: production never called `set_event_sink`, so the
    /// provider's `ApiRetry` heartbeats (item 49) were dead code — an
    /// unattended rate-limit wait rendered as a stalled/dead ingot. The
    /// cancel flag has the same wiring requirement.
    #[test]
    fn build_provider_wires_heartbeat_sink_and_cancel_flag() {
        let hooks = EngineHooks {
            events: None,
            steer: None,
            cancel: Some(crate::engine::CancelFlag::default()),
        };
        let smith = NativeSmith::forge(cfg(), "rust", 1, PathBuf::from("."), &hooks);
        let (tx, _rx) = events::channel();
        let provider = smith.build_provider(&tx);
        assert!(provider.has_event_sink(), "ApiRetry heartbeats need a sink");
        assert!(provider.has_cancel_flag(), "Ctrl-C must reach retry waits");

        // Without a cancel hook the sink is still wired.
        let smith = NativeSmith::forge(cfg(), "rust", 1, PathBuf::from("."), &EngineHooks::default());
        let provider = smith.build_provider(&tx);
        assert!(provider.has_event_sink());
        assert!(!provider.has_cancel_flag());
    }

    #[test]
    fn cast_pins_model_and_sheds_steer() {
        let steer = crate::engine::SteerQueue::default();
        let cancel = crate::engine::CancelFlag::default();
        let hooks = EngineHooks {
            events: None,
            steer: Some(steer),
            cancel: Some(cancel),
        };
        // Grade 5 would normally select the plan model; the pin wins.
        let smith = NativeSmith::cast(cfg(), "web", 5, PathBuf::from("."), "alt/model", &hooks);
        assert_eq!(smith.model(), "alt/model");
        assert_eq!(smith.mode, PromptMode::Forge);
        assert!(smith.hooks.steer.is_none(), "casts must not consume dashboard steers");
        assert!(smith.hooks.cancel.is_some());
    }

    #[test]
    fn plan_and_work_roles_use_separate_providers_without_router_calls() {
        let (_guard, _dir) = crate::config::isolated_config_dir();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use wiremock::matchers::{any, body_partial_json, header, method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let plan = MockServer::start().await;
            let work = MockServer::start().await;
            let router = MockServer::start().await;
            Mock::given(any())
                .respond_with(ResponseTemplate::new(500))
                .expect(0).mount(&router).await;
            for (server, key, model, answer) in [
                (&plan, "plan-key", "gpt-4.1-mini", "planned"),
                (&work, "work-key", "gpt-oss-120b", "forged\nCMD: true"),
            ] {
                Mock::given(method("POST"))
                    .and(path("/chat/completions"))
                    .and(header("Authorization", format!("Bearer {key}")))
                    .and(body_partial_json(serde_json::json!({"model": model})))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "choices": [{"message": {"content": answer}, "finish_reason": "stop"}],
                    })))
                    .expect(1).mount(server).await;
            }
            let vars = [
                ("OPENAI_API_KEY", "plan-key".to_string()),
                ("CEREBRAS_API_KEY", "work-key".to_string()),
                ("SLAG_OPENAI_BASE_URL", plan.uri()),
                ("SLAG_CEREBRAS_BASE_URL", work.uri()),
            ];
            let previous: Vec<_> = vars.iter().map(|(name, _)| (*name, std::env::var_os(name))).collect();
            for (name, value) in &vars {
                std::env::set_var(name, value);
            }
            let mut config = cfg();
            config.base_url = router.uri();
            config.model_base = "cerebras::gpt-oss-120b".into();
            config.model_plan = "openai::gpt-4.1-mini".into();
            let hooks = EngineHooks::default();
            let planner = NativeSmith::plan(config.clone(), PathBuf::from("."), &hooks);
            let worker = NativeSmith::forge(config, "rust", 1, PathBuf::from("."), &hooks);
            let planned = planner.invoke("Return planned.").await;
            let forged = worker.invoke("Return forged.").await;
            for (name, value) in previous {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            assert_eq!(planned.unwrap(), "planned");
            assert_eq!(forged.unwrap(), "forged\nCMD: true");
            assert!(router.received_requests().await.unwrap().is_empty());
            for server in [&plan, &work] {
                let requests = server.received_requests().await.unwrap();
                assert_eq!(requests.len(), 1);
                assert!(!requests[0].headers.contains_key("HTTP-Referer"));
                let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
                assert!(body.get("reasoning").is_none());
            }
        });
    }
}
