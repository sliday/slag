pub mod mock;
pub mod native;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use crate::config::EngineConfig;
use crate::error::SlagError;

/// Async trait for invoking an AI smith (Claude or mock).
/// Uses boxed future for dyn compatibility.
pub trait Smith: Send + Sync {
    /// Send a prompt and receive the response text.
    fn invoke(&self, prompt: &str) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>>;
}

/// TUI hooks threaded from `main` down to the native agent loop.
/// `events` fans the engine event stream out to the dashboard (the
/// per-invocation JSONL sink keeps running regardless); `steer` and
/// `cancel` wire the dashboard's input line and Ctrl-C into the agent.
#[derive(Clone, Default)]
pub struct EngineHooks {
    pub events: Option<crate::engine::EventTx>,
    pub steer: Option<crate::engine::SteerQueue>,
    pub cancel: Option<crate::engine::CancelFlag>,
}

/// Workspace root for native smiths: the current directory (forge runs
/// in the project root; anvil worktrees chdir before invoking).
fn workspace_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Forge smith factory. One engine, one provider: OpenRouter.
pub fn make_smith(cfg: &EngineConfig, skill: &str, grade: u8, hooks: &EngineHooks) -> Box<dyn Smith> {
    Box::new(native::NativeSmith::forge(
        cfg.clone(),
        skill,
        grade,
        workspace_root(),
        hooks,
    ))
}

/// Forge smith factory sharing an existing ingot spend accumulator.
/// The duel fallback strike uses this so the single-smith path continues
/// the ingot's budget instead of restarting from $0.
pub fn make_smith_with_spend(
    cfg: &EngineConfig,
    skill: &str,
    grade: u8,
    hooks: &EngineHooks,
    spend: crate::engine::agent::SpendAccum,
) -> Box<dyn Smith> {
    Box::new(
        native::NativeSmith::forge(cfg.clone(), skill, grade, workspace_root(), hooks)
            .with_ingot_spend(spend),
    )
}

/// Plan smith factory (surveyor/founder passes). `role` splits the two
/// phases on the cost ledger, which matters because they share a model.
pub fn make_plan_smith(
    cfg: &EngineConfig,
    hooks: &EngineHooks,
    role: crate::engine::Role,
) -> Box<dyn Smith> {
    Box::new(native::NativeSmith::plan(cfg.clone(), workspace_root(), hooks).with_role(role))
}

/// Base smith factory (resmelt analysis — low grade, no skill).
/// Runs in plan mode: resmelt is an analysis-only pass whose text output is
/// parsed as REWRITE/SPLIT/IMPOSSIBLE s-expressions, so it must not get
/// forge-mode write access or finish-summary-only output.
pub fn make_base_smith(cfg: &EngineConfig, hooks: &EngineHooks) -> Box<dyn Smith> {
    Box::new(native::NativeSmith::plan(cfg.clone(), workspace_root(), hooks))
}

/// Check if response text contains unresolved questions
pub fn has_questions(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with('?') {
            return true;
        }
        if trimmed.starts_with("**Question")
            || trimmed.starts_with("Question")
            || trimmed.starts_with("Which ")
            || trimmed.starts_with("What ")
            || trimmed.starts_with("Should ")
            || trimmed.starts_with("Do you ")
            || trimmed.starts_with("Would you ")
            || trimmed.starts_with("Can you ")
            || trimmed.starts_with("Could you ")
        {
            return true;
        }
    }
    false
}

/// Self-iterate to resolve questions in smith output.
pub async fn self_iterate(
    smith: &dyn Smith,
    mut raw: String,
    max_iter: usize,
) -> Result<String, SlagError> {
    let mut iterations = 0;
    while has_questions(&raw) && iterations < max_iter {
        iterations += 1;
        let follow_up = format!(
            "{raw}\n\n---\n[SELF-QUERY RESOLUTION]\n\
            You asked questions above. You are the expert. Answer them yourself:\n\
            - Make decisive choices based on best practices\n\
            - Choose the most sensible option when uncertain\n\
            - Do not ask for clarification - decide and proceed\n\n\
            Now output the COMPLETE deliverable with all decisions made.\n\
            NO QUESTIONS. NO PREAMBLE. Just the final output."
        );
        raw = smith.invoke(&follow_up).await?;
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_hooks_default_is_inert() {
        let hooks = EngineHooks::default();
        assert!(hooks.events.is_none());
        assert!(hooks.steer.is_none());
        assert!(hooks.cancel.is_none());
        // Clones stay cheap and independent.
        let clone = hooks.clone();
        assert!(clone.events.is_none());
    }

    fn openrouter_only_config(base_url: String) -> EngineConfig {
        EngineConfig {
            api_key: "sk-or-test".into(),
            model_base: "test/base".into(),
            model_plan: "test/plan".into(),
            model_alt: "test/alt".into(),
            model_judge: "test/judge".into(),
            effort: None,
            base_url,
            duel: crate::config::DuelMode::Auto,
            duel_rounds_override: None,
            screenshot_cmd: None,
        }
    }

    /// slag has one provider. All three factories take the config by
    /// reference (one config serves the whole run) and every smith they
    /// build talks OpenRouter over HTTP — there is no Claude CLI branch
    /// left to fall back to, so the mock server sees every call.
    #[tokio::test]
    async fn factories_build_openrouter_smiths_from_one_borrowed_config() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": { "content": "forged" },
                        "finish_reason": "stop",
                    }],
                })),
            )
            .mount(&server)
            .await;

        let cfg = openrouter_only_config(server.uri());
        let hooks = EngineHooks::default();

        // One borrow feeds all three: no factory may consume the config.
        let forge = make_smith(&cfg, "rust", 1, &hooks);
        let plan = make_plan_smith(&cfg, &hooks, crate::engine::Role::Plan);
        let base = make_base_smith(&cfg, &hooks);
        assert_eq!(cfg.model_base, "test/base", "config survives the factories");

        for smith in [&forge, &plan, &base] {
            assert_eq!(smith.invoke("do the thing").await.unwrap(), "forged");
        }

        // Model per role: forge at grade 1 works on base, both plan-mode
        // factories reason on the plan model.
        // Each invoke also fetches GET /models for the context window;
        // only the chat calls carry a JSON body with a model id.
        let models: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|req| req.url.path().ends_with("/chat/completions"))
            .map(|req| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                body["model"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(models, ["test/base", "test/plan", "test/plan"]);
    }

    #[tokio::test]
    async fn make_smith_with_spend_shares_the_ingot_accumulator() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": { "content": "forged" },
                        "finish_reason": "stop",
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15,
                        "cost": 0.25,
                    },
                })),
            )
            .mount(&server)
            .await;

        // The duel-fallback factory must continue the shared ingot spend,
        // not restart from a fresh $0 accumulator.
        let acc = crate::engine::agent::SpendAccum::default();
        *acc.lock().unwrap() = 0.50;
        let cfg = openrouter_only_config(server.uri());
        let smith = make_smith_with_spend(&cfg, "rust", 1, &EngineHooks::default(), acc.clone());
        assert_eq!(smith.invoke("do the thing").await.unwrap(), "forged");
        assert!(
            (*acc.lock().unwrap() - 0.75).abs() < 1e-9,
            "session cost folds into the shared accumulator"
        );
    }

    #[test]
    fn detect_questions() {
        assert!(has_questions("What framework should we use?"));
        assert!(has_questions("**Question**: which approach?"));
        assert!(has_questions("Should we use React or Vue?"));
        assert!(!has_questions("# Blueprint\nThis is a plan."));
        assert!(!has_questions("Create the file structure."));
    }
}
