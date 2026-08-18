//! NativeSmith — engine-backed Smith. The internal agent loop replaces the
//! `claude -p` subprocess: same `Smith` boundary, zero external CLIs.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use super::{EngineHooks, Smith};
use crate::config::EngineConfig;
use crate::engine::agent::{ForgeAgent, SpendAccum};
use crate::engine::events::{self, StderrNarrator};
use crate::engine::prompt::{self, PromptMode};
use crate::engine::provider::OpenRouter;
use crate::engine::tools::ToolBox;
use crate::engine::Effort;
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
        }
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
        }
    }

    /// Share a spend accumulator with other smiths of the same ingot
    /// (duel casts are rebuilt every round; the accumulator persists).
    pub fn with_ingot_spend(mut self, acc: SpendAccum) -> Self {
        self.ingot_spend = acc;
        self
    }

    fn model(&self) -> &str {
        if let Some(model) = &self.model_override {
            return model;
        }
        match self.mode {
            PromptMode::Plan => &self.cfg.model_plan,
            PromptMode::Forge => self.cfg.model_for_grade(self.grade),
        }
    }

    async fn invoke_impl(&self, prompt_text: &str) -> Result<String, SlagError> {
        let model = self.model().to_string();
        let bands = prompt::build(&self.root, &model, self.mode);
        let effort = self.cfg.effort.or(Some(Effort::from_grade(self.grade)));

        // One agent channel fans out to the JSONL sink, the display, and
        // (when the TUI is up) the dashboard hook. The dashboard replaces
        // the stderr narrator: both writing to the terminal at once would
        // corrupt the alternate screen.
        let (tx, mut rx) = events::channel();
        let (tx_jsonl, rx_jsonl) = events::channel();
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
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
                    let _ = narr.send(event);
                }
            }
        });

        let provider = Arc::new(OpenRouter::with_base_url(
            self.cfg.api_key.clone(),
            self.cfg.base_url.clone(),
        ));
        // Size the compaction budget to the model's real window (cached
        // per model on the provider; `None` on fetch failure keeps the
        // default): a 32k model compacts before it 400s, a 1M model does
        // not throw context away at the fixed default.
        let window = provider.context_length(&model).await;
        let mut agent = ForgeAgent::new(provider, ToolBox::new(&self.root), &model)
            .with_context_window(window)
            .with_effort(effort)
            .with_ingot_spend(self.ingot_spend.clone())
            .with_events(tx);
        if let Some(steer) = &self.hooks.steer {
            agent = agent.with_steer(steer.clone());
        }
        if let Some(cancel) = &self.hooks.cancel {
            agent = agent.with_cancel(cancel.clone());
        }

        let result = agent.run(bands.join(), prompt_text.to_string()).await;

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
                    "message": { "content": "done" },
                    "finish_reason": "stop",
                }],
            })))
            .mount(&server)
            .await;

        let mut config = cfg();
        config.base_url = server.uri();
        let smith =
            NativeSmith::forge(config, "rust", 1, PathBuf::from("."), &EngineHooks::default());
        assert_eq!(smith.invoke("task").await.unwrap(), "done");
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
}
