//! engine — slag's native forging engine.
//!
//! OpenRouter-backed agentic loop. This module replaces external CLI smiths.
//! Shared contracts live here; submodules implement them.
#![allow(dead_code)]

pub mod agent;
pub mod compact;
pub mod events;
pub mod mcp;
pub mod policy;
pub mod prompt;
pub mod provider;
pub mod tools;
pub mod transcript;

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::error::SlagError;

pub const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";

/// Reasoning effort forwarded to OpenRouter's unified `reasoning` param.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn from_grade(grade: u8) -> Self {
        match grade {
            0..=2 => Effort::Low,
            3 => Effort::Medium,
            _ => Effort::High,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

/// A tool invocation as it appears on the OpenAI-compatible wire.
/// `arguments` stays a raw JSON string; `tools::ToolBox` parses it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// One chat message. Wire-compatible with OpenAI chat/completions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Base64 data URLs. Never serialized directly: the provider expands
    /// content into multimodal parts when images are present.
    #[serde(skip)]
    pub images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Raw OpenRouter reasoning blocks. Anthropic/Gemini reasoning models
    /// require these replayed verbatim with the assistant tool_calls
    /// message, or the follow-up request is rejected.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reasoning_details: Option<serde_json::Value>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into(), tool_calls: None, tool_call_id: None, reasoning_details: None, images: None }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into(), tool_calls: None, tool_call_id: None, reasoning_details: None, images: None }
    }
    pub fn assistant(content: impl Into<String>, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self { role: "assistant".into(), content: content.into(), tool_calls, tool_call_id: None, reasoning_details: None, images: None }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            reasoning_details: None,
            images: None,
        }
    }
    pub fn with_reasoning_details(mut self, details: Option<serde_json::Value>) -> Self {
        self.reasoning_details = details;
        self
    }
}

/// Tool schema advertised to the model.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// OpenRouter reports spend in credits when `usage: {include: true}`.
    #[serde(default)]
    pub cost: Option<f64>,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        if let Some(c) = other.cost {
            *self.cost.get_or_insert(0.0) += c;
        }
    }
}

/// Provider-agnostic model response (hermes `NormalizedResponse` pattern).
#[derive(Debug, Clone)]
pub struct NormalizedResponse {
    /// Model the provider actually ran, when it says. Differs from the
    /// requested id whenever a router like `openrouter/auto` is in play.
    pub model: Option<String>,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub reasoning: Option<String>,
    /// Raw reasoning blocks, preserved for replay (see `ChatMessage`).
    pub reasoning_details: Option<serde_json::Value>,
    pub usage: Usage,
}

/// One model call.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub effort: Option<Effort>,
    pub max_tokens: Option<u32>,
}

/// Model provider boundary. Boxed future for dyn compatibility,
/// same style as `crate::smith::Smith`.
pub trait Provider: Send + Sync {
    fn chat(
        &self,
        req: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>;

    /// Hand the provider an event channel for retry heartbeats
    /// (`ApiRetry`). Default no-op: mock providers and wrappers that do
    /// not sleep have nothing to report.
    fn set_event_sink(&self, _tx: EventTx) {}

    /// Hand the provider the run's cancel flag so a Ctrl-C aborts a
    /// retry wait instead of sleeping it out (and firing more requests).
    /// Default no-op: mock providers and wrappers do not sleep.
    fn set_cancel_flag(&self, _f: CancelFlag) {}
}

/// Forwarding impl so borrowed providers (`&P`, `&dyn Provider`) can be
/// handed to adapters like `agent::SpendTracked` without cloning an Arc.
impl<P: Provider + ?Sized> Provider for &P {
    fn chat(
        &self,
        req: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>> {
        (**self).chat(req)
    }

    fn set_event_sink(&self, tx: EventTx) {
        (**self).set_event_sink(tx)
    }

    fn set_cancel_flag(&self, f: CancelFlag) {
        (**self).set_cancel_flag(f)
    }
}

/// Typed engine events. One stream feeds TUI, JSONL logs, and --json mode.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    TurnStart { turn: usize },
    ModelCall { model: String },
    /// A router answered with a different model than the one requested.
    /// Only `openrouter/auto` and friends emit this; a pinned model never
    /// does, so this line always carries news.
    ModelRouted { requested: String, routed: String },
    ToolCallStart { name: String, preview: String },
    ToolResult { name: String, ok: bool, preview: String },
    Tokens { usage: Usage },
    Steer { text: String },
    Finish { summary: String },
    Error { message: String },
    /// Plain-language interpretation of the current activity, produced by
    /// the narrator model. Display-only; never enters the smith's context.
    Narrate { text: String },
    /// Soft alert (e.g. spend at 80% of budget). Display + JSONL only.
    Warning { message: String },
    /// Retry heartbeat: the provider is waiting out a transient failure.
    /// Long unattended waits are chunked into slices, one heartbeat per
    /// slice, so the dashboard and JSONL logs stay alive through a
    /// minutes-long rate-limit window.
    ApiRetry { attempt: usize, status: u16, remaining_secs: u64 },
    // Pipeline-level events (emitted by forge, consumed by the dashboard).
    IngotStart { id: String, work: String },
    HeatTick { id: String, heat: u8 },
    IngotDone { id: String, ok: bool },
    DuelRound { id: String, round: u8 },
    DuelVerdict { id: String, winner: char, margin: u8 },
}

/// Steering messages queued by the TUI, drained by the agent loop before
/// each model call (hermes steer-into-tool-result pattern).
pub type SteerQueue = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// Hard-interrupt flag checked at each turn boundary.
pub type CancelFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;

/// Assayer verdict for one duel round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// 'a' or 'b'
    pub winner: char,
    pub score_a: u8,
    pub score_b: u8,
    /// What the loser did better — injected into the next round.
    pub critique: String,
}

impl Verdict {
    pub fn margin(&self) -> u8 {
        self.score_a.abs_diff(self.score_b)
    }
}

pub type EventTx = tokio::sync::mpsc::UnboundedSender<EngineEvent>;

/// Emit an event, ignoring a closed channel (engine must never die on display).
pub fn emit(tx: &Option<EventTx>, event: EngineEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Outcome of one tool dispatch.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
}

/// Run-wide performance and failure accounting for the assay report
/// (items 87 + 89): wall vs API vs tool durations (retry overhead split
/// out) and per-tool error tallies with coarse error classes. One global
/// accumulator, same pattern as `config`'s run-spend counter — the assay
/// printer has no runtime handle to the agents that did the work.
pub mod stats {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Per-tool error tally: total failures plus counts per coarse class.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ToolErrors {
        pub total: usize,
        pub classes: BTreeMap<&'static str, usize>,
    }

    /// Snapshot of the run's accounting.
    #[derive(Debug, Clone, Default)]
    pub struct RunStats {
        /// Time spent inside `provider.chat()` (first attempts).
        pub api: Duration,
        /// Additional chat time from in-session retries (overflow shrink
        /// retries, continuation re-calls). Provider-internal transient
        /// waits are invisible here and count into `api`.
        pub retries: Duration,
        /// Wall time of tool batches (parallel readers count once).
        pub tools: Duration,
        /// Wall clock since `mark_run_start`.
        pub wall: Option<Duration>,
        pub tool_errors: BTreeMap<String, ToolErrors>,
    }

    struct Cell {
        api: Duration,
        retries: Duration,
        tools: Duration,
        run_started: Option<Instant>,
        tool_errors: BTreeMap<String, ToolErrors>,
    }

    static CELL: Mutex<Cell> = Mutex::new(Cell {
        api: Duration::ZERO,
        retries: Duration::ZERO,
        tools: Duration::ZERO,
        run_started: None,
        tool_errors: BTreeMap::new(),
    });

    fn cell() -> std::sync::MutexGuard<'static, Cell> {
        CELL.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Forge start: anchors the wall clock. First call wins.
    pub fn mark_run_start() {
        let mut c = cell();
        if c.run_started.is_none() {
            c.run_started = Some(Instant::now());
        }
    }

    /// One `provider.chat()` call took `d`; `retry` marks attempts after
    /// the first within one send (context-overflow retries and the like).
    pub fn record_api(d: Duration, retry: bool) {
        let mut c = cell();
        if retry {
            c.retries += d;
        } else {
            c.api += d;
        }
    }

    /// One tool batch took `d` of wall time.
    pub fn record_tools(d: Duration) {
        cell().tools += d;
    }

    /// One tool call failed; classify from the error text.
    pub fn record_tool_error(tool: &str, output: &str) {
        let class = classify_error(output);
        let mut c = cell();
        let e = c.tool_errors.entry(tool.to_string()).or_default();
        e.total += 1;
        *e.classes.entry(class).or_insert(0) += 1;
    }

    pub fn snapshot() -> RunStats {
        let c = cell();
        RunStats {
            api: c.api,
            retries: c.retries,
            tools: c.tools,
            wall: c.run_started.map(|t| t.elapsed()),
            tool_errors: c.tool_errors.clone(),
        }
    }

    /// Coarse error class from a tool error's text. The buckets mirror
    /// the failure modes the fuzzy edit ladder and the sandbox produce,
    /// so 'edit_file 7 (no-match 5)' reads directly as "the ladder is
    /// thrashing".
    pub fn classify_error(output: &str) -> &'static str {
        let s = output.to_ascii_lowercase();
        if s.contains("no match") || s.contains("no exact match") {
            "no-match"
        } else if s.contains("matches") && s.contains("times") {
            "multi-match"
        } else if s.contains("timed out") {
            "timeout"
        } else if s.contains("escapes workspace") {
            "sandbox"
        } else if s.contains("not been read this session")
            || s.contains("stale write")
            || s.contains("changed on disk")
        {
            "stale-read"
        } else if s.contains("exit code") || s.contains("exited with") {
            "nonzero-exit"
        } else {
            "other"
        }
    }

    /// `252s` → `4m12s`; hours only when they exist.
    pub fn fmt_dur(d: Duration) -> String {
        let total = d.as_secs();
        let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
        if h > 0 {
            format!("{h}h{m:02}m")
        } else if m > 0 {
            format!("{m}m{s:02}s")
        } else {
            format!("{s}s")
        }
    }

    /// `wall 4m12s · api 2m01s (retries +18s) · tools 1m40s`, or `None`
    /// when the run was never marked (nothing to report).
    pub fn durations_line(stats: &RunStats) -> Option<String> {
        let wall = stats.wall?;
        let api_total = stats.api + stats.retries;
        let mut line = format!("wall {} · api {}", fmt_dur(wall), fmt_dur(api_total));
        if !stats.retries.is_zero() {
            line.push_str(&format!(" (retries +{})", fmt_dur(stats.retries)));
        }
        line.push_str(&format!(" · tools {}", fmt_dur(stats.tools)));
        Some(line)
    }

    /// `tool errors: edit_file 7 (no-match 5), bash 2`, or `None` for a
    /// clean run. Tools sort by failure count; classes ride along except
    /// an uninformative all-"other" tally.
    pub fn tool_errors_line(stats: &RunStats) -> Option<String> {
        if stats.tool_errors.is_empty() {
            return None;
        }
        let mut tools: Vec<(&String, &ToolErrors)> = stats.tool_errors.iter().collect();
        tools.sort_by(|a, b| b.1.total.cmp(&a.1.total).then_with(|| a.0.cmp(b.0)));
        let parts: Vec<String> = tools
            .into_iter()
            .map(|(name, e)| {
                let named: Vec<String> = e
                    .classes
                    .iter()
                    .filter(|(class, _)| **class != "other")
                    .map(|(class, n)| format!("{class} {n}"))
                    .collect();
                if named.is_empty() {
                    format!("{name} {}", e.total)
                } else {
                    format!("{name} {} ({})", e.total, named.join(", "))
                }
            })
            .collect();
        Some(format!("tool errors: {}", parts.join(", ")))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn error_classes_map_the_known_failure_modes() {
            assert_eq!(classify_error("no match for old_string in src/x.rs"), "no-match");
            assert_eq!(
                classify_error("no exact match for old_string in a.rs; fuzzy refused"),
                "no-match"
            );
            assert_eq!(
                classify_error("old_string matches 3 times in a.rs (lines 1, 2, 9)"),
                "multi-match"
            );
            assert_eq!(classify_error("command timed out after 120s"), "timeout");
            assert_eq!(classify_error("path escapes workspace: ../../etc"), "sandbox");
            assert_eq!(
                classify_error("edit_file: a.rs exists but has not been read this session"),
                "stale-read"
            );
            assert_eq!(
                classify_error("refused stale write; a.rs changed on disk since your last read"),
                "stale-read"
            );
            assert_eq!(classify_error("something novel"), "other");
        }

        #[test]
        fn durations_line_splits_wall_api_retries_tools() {
            let stats = RunStats {
                api: Duration::from_secs(103),
                retries: Duration::from_secs(18),
                tools: Duration::from_secs(100),
                wall: Some(Duration::from_secs(252)),
                tool_errors: BTreeMap::new(),
            };
            assert_eq!(
                durations_line(&stats).unwrap(),
                "wall 4m12s · api 2m01s (retries +18s) · tools 1m40s"
            );

            // No retries: the parenthetical disappears.
            let clean = RunStats { retries: Duration::ZERO, ..stats.clone() };
            assert_eq!(
                durations_line(&clean).unwrap(),
                "wall 4m12s · api 1m43s · tools 1m40s"
            );
            // Unmarked run: nothing to report.
            assert!(durations_line(&RunStats::default()).is_none());
        }

        #[test]
        fn tool_errors_line_ranks_tools_and_names_classes() {
            let mut stats = RunStats::default();
            let edit = stats.tool_errors.entry("edit_file".into()).or_default();
            edit.total = 7;
            edit.classes.insert("no-match", 5);
            edit.classes.insert("other", 2);
            let bash = stats.tool_errors.entry("bash".into()).or_default();
            bash.total = 2;
            bash.classes.insert("other", 2);

            assert_eq!(
                tool_errors_line(&stats).unwrap(),
                "tool errors: edit_file 7 (no-match 5), bash 2"
            );
            assert!(tool_errors_line(&RunStats::default()).is_none());
        }

        #[test]
        fn fmt_dur_scales() {
            assert_eq!(fmt_dur(Duration::from_secs(9)), "9s");
            assert_eq!(fmt_dur(Duration::from_secs(61)), "1m01s");
            assert_eq!(fmt_dur(Duration::from_secs(3723)), "1h02m");
        }

        #[test]
        fn global_accumulator_folds_api_retry_tool_and_error_records() {
            // Other tests share the process-global cell, so assert
            // deltas, never absolutes.
            let before = snapshot();
            record_api(Duration::from_millis(40), false);
            record_api(Duration::from_millis(10), true);
            record_tools(Duration::from_millis(25));
            record_tool_error("edit_file", "no match for old_string in x.rs");
            let after = snapshot();
            assert!(after.api >= before.api + Duration::from_millis(40));
            assert!(after.retries >= before.retries + Duration::from_millis(10));
            assert!(after.tools >= before.tools + Duration::from_millis(25));
            let edit = after.tool_errors.get("edit_file").unwrap();
            let edit_before = before.tool_errors.get("edit_file").cloned().unwrap_or_default();
            assert!(edit.total >= edit_before.total + 1);
            assert!(
                edit.classes.get("no-match").copied().unwrap_or(0)
                    >= edit_before.classes.get("no-match").copied().unwrap_or(0) + 1
            );
        }
    }
}
