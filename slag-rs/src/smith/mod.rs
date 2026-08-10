pub mod claude;
pub mod mock;
pub mod native;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use crate::config::{EngineConfig, SmithConfig};
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
/// `ClaudeSmith` ignores all three.
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

/// Forge smith factory: native engine when an OpenRouter key is configured,
/// else the Claude CLI (current behavior).
pub fn make_smith(config: &SmithConfig, skill: &str, grade: u8, hooks: &EngineHooks) -> Box<dyn Smith> {
    match EngineConfig::load() {
        Some(cfg) => Box::new(native::NativeSmith::forge(cfg, skill, grade, workspace_root(), hooks)),
        None => Box::new(claude::ClaudeSmith::from_config(config, skill, grade)),
    }
}

/// Plan smith factory (surveyor/founder passes).
pub fn make_plan_smith(config: &SmithConfig, hooks: &EngineHooks) -> Box<dyn Smith> {
    match EngineConfig::load() {
        Some(cfg) => Box::new(native::NativeSmith::plan(cfg, workspace_root(), hooks)),
        None => Box::new(claude::ClaudeSmith::plan(config)),
    }
}

/// Base smith factory (resmelt analysis — low grade, no skill).
/// Native engine runs this in plan mode: resmelt is an analysis-only pass
/// whose text output is parsed as REWRITE/SPLIT/IMPOSSIBLE s-expressions,
/// so it must not get forge-mode write access or finish-summary-only output.
pub fn make_base_smith(config: &SmithConfig, hooks: &EngineHooks) -> Box<dyn Smith> {
    match EngineConfig::load() {
        Some(cfg) => Box::new(native::NativeSmith::plan(cfg, workspace_root(), hooks)),
        None => Box::new(claude::ClaudeSmith::base(config)),
    }
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
        // ClaudeSmith ignores hooks entirely: the CLI path still builds
        // from SmithConfig alone.
        let config = SmithConfig::from_env();
        let _cli: Box<dyn Smith> = Box::new(claude::ClaudeSmith::from_config(&config, "web", 1));
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
