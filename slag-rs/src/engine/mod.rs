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
pub mod pricing;
pub mod prompt;
pub mod provider;
pub mod tools;
pub mod transcript;
pub mod hooks;

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

/// Which call site spent the tokens. Carried on `ChatRequest` and copied
/// onto the `Usage` that comes back, so the ledger can split smith spend
/// from judge, founder and surveyor spend after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Smith,
    Plan,
    Judge,
    Founder,
    Surveyor,
    Compact,
    Duel,
    /// The goal critic. Its own role so the ledger says what the goal
    /// checks cost: billed as a smith, they hide inside forge spend, and
    /// a user cannot decide whether tempering is worth it.
    Warden,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Smith => "smith",
            Role::Plan => "plan",
            Role::Judge => "judge",
            Role::Founder => "founder",
            Role::Surveyor => "surveyor",
            Role::Compact => "compact",
            Role::Duel => "duel",
            Role::Warden => "warden",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
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
    /// True when `cost` came from the local pricing table rather than the
    /// provider, so readouts can mark the number as an estimate.
    #[serde(default)]
    pub estimated: bool,
    /// Model that actually ran, and the call site that asked. Together they
    /// key the cost ledger; a fold across several calls leaves both `None`.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        if let Some(c) = other.cost {
            *self.cost.get_or_insert(0.0) += c;
        }
        // One estimated leg makes the whole sum an estimate. The key fields
        // stay put: a fold across models has no single model or role.
        self.estimated |= other.estimated;
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

/// How hard one request is allowed to retry. A forge strike is the work
/// itself and rides the provider-wide budget. A side call — duel judging,
/// a recipe suggestion, an assay summary — is advisory, so it takes one
/// swing: retrying it during a capacity event multiplies load across all
/// `MAX_ANVILS` workers to buy nothing the run needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempt budget for this request. `None` defers to the provider's.
    pub attempts: Option<usize>,
    /// Whether capacity errors may retry for free in unattended mode.
    pub persistent: bool,
}

impl RetryPolicy {
    /// The provider-wide budget, unattended waiting included.
    pub const fn full() -> Self {
        Self { attempts: None, persistent: true }
    }

    /// One swing, and never wait out a capacity event.
    pub const fn side() -> Self {
        Self { attempts: Some(1), persistent: false }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::full()
    }
}

/// One model call.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub effort: Option<Effort>,
    pub max_tokens: Option<u32>,
    /// Which call site this is. Rides back on `Usage.role` so the ledger
    /// can attribute spend without the caller threading a second channel.
    pub role: Role,
    /// How hard this one request retries (item 50).
    pub retry: RetryPolicy,
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
    /// `lines`/`bytes` measure the tool's *full* output, not the truncated
    /// `preview`, so a collapsed one-liner can honestly say how much it is
    /// hiding; `ms` is the dispatch wall time. All three default, so a
    /// JSONL line written before they existed still deserializes.
    ToolResult {
        name: String,
        ok: bool,
        preview: String,
        #[serde(default)]
        lines: usize,
        #[serde(default)]
        bytes: usize,
        #[serde(default)]
        ms: u64,
    },
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
    /// How full the smith's context is at a turn boundary. `budget_tokens`
    /// is already net of the output reserve and the compaction buffer, so
    /// 100% is the compaction trigger, not the raw model window.
    ContextGauge { pct: u8, used_tokens: u64, budget_tokens: usize },
    /// A lifecycle hook started. Paired with `HookFinished`, this is what
    /// keeps a slow hook from reading as a hung smith.
    HookStarted { name: String, hook_event: String, status_message: Option<String> },
    /// A lifecycle hook returned. `code` follows the exit-code protocol:
    /// 0 injected context, 2 blocked, `-1` never produced a status.
    HookFinished { name: String, hook_event: String, code: i32, duration_ms: u64 },
}

/// Steering messages queued by the TUI, drained by the agent loop before
/// each model call (hermes steer-into-tool-result pattern).
pub type SteerQueue = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// Hard-interrupt flag. Four surfaces set it (dashboard, TUI, signal
/// handler, tests), so it stays the bare shared bool it has always been;
/// item 56's reason rides alongside in `Cancellation` rather than
/// replacing it.
pub type CancelFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;

/// Why a run stopped (item 56). A user abort is a failure the assay should
/// report; a steer interrupt is the operator redirecting live work and
/// must not read as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CancelReason {
    #[default]
    UserAbort,
    SteerInterrupt,
}

impl CancelReason {
    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::SteerInterrupt,
            _ => Self::UserAbort,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::UserAbort => 0,
            Self::SteerInterrupt => 1,
        }
    }

    /// How the interrupt reads in an event or an assay line.
    pub fn label(self) -> &'static str {
        match self {
            Self::UserAbort => "cancelled by operator",
            Self::SteerInterrupt => "interrupted to steer",
        }
    }
}

/// A `CancelFlag` with the reason it was raised (item 56). Cloning shares
/// both, so a handler that raises it and a loop that reads it see the same
/// state. `flag` is the same `CancelFlag` every existing caller already
/// holds, so nothing that only needs the boolean has to change.
#[derive(Debug, Clone, Default)]
pub struct Cancellation {
    pub flag: CancelFlag,
    reason: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl Cancellation {
    /// Adopt an existing flag — the bridge for the four surfaces that
    /// already own a `CancelFlag` and cannot be rewritten at once.
    pub fn from_flag(flag: CancelFlag) -> Self {
        Self { flag, ..Self::default() }
    }

    /// Raise the interrupt. The first reason wins: a steer that arrives
    /// after a ctrl-C must not relabel the abort as a redirect.
    pub fn raise(&self, reason: CancelReason) {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.flag.swap(true, SeqCst) {
            self.reason.store(reason.code(), SeqCst);
        }
    }

    pub fn is_raised(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn reason(&self) -> CancelReason {
        CancelReason::from_code(self.reason.load(std::sync::atomic::Ordering::SeqCst))
    }
}

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

    /// Lines added and removed by one ingot's file writes.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Churn {
        pub added: usize,
        pub removed: usize,
    }

    /// Bucket for writes made outside any ingot attempt.
    pub const CHURN_UNATTRIBUTED: &str = "-";

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
        /// Line churn per ingot id. Ingots that never wrote are absent;
        /// writes outside an ingot (plan passes, duel casts) land under
        /// `CHURN_UNATTRIBUTED`.
        pub churn: BTreeMap<String, Churn>,
        /// Spend split per model and per call site (item 35), so judge and
        /// duel cost stop hiding inside one session total.
        pub ledger: super::pricing::CostLedger,
    }

    struct Cell {
        api: Duration,
        retries: Duration,
        tools: Duration,
        run_started: Option<Instant>,
        tool_errors: BTreeMap<String, ToolErrors>,
        churn: BTreeMap<String, Churn>,
        ledger: super::pricing::CostLedger,
    }

    static CELL: Mutex<Cell> = Mutex::new(Cell {
        api: Duration::ZERO,
        retries: Duration::ZERO,
        tools: Duration::ZERO,
        run_started: None,
        tool_errors: BTreeMap::new(),
        churn: BTreeMap::new(),
        ledger: super::pricing::CostLedger::new(),
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

    /// Fold one call's usage into the run ledger (item 35). Keyed by the
    /// `model`/`role` the provider stamped on it, so a duel cast against
    /// the alt model and the judge call that scored it land on separate
    /// rows even though both happen inside one ingot.
    pub fn record_usage(usage: &super::Usage) {
        cell().ledger.fold(usage);
    }

    /// Lines `new` adds and drops relative to `old`, as a multiset delta
    /// over lines. A rewritten line reads as one added and one removed; an
    /// appended block as N added and none removed; a block moved without
    /// edits as neither, which is the honest churn answer. O(n), and no
    /// LCS table — this runs on every write in the hot path.
    pub fn line_churn(old: &str, new: &str) -> Churn {
        let mut tally: BTreeMap<&str, i64> = BTreeMap::new();
        for line in old.lines() {
            *tally.entry(line).or_insert(0) -= 1;
        }
        for line in new.lines() {
            *tally.entry(line).or_insert(0) += 1;
        }
        let mut churn = Churn::default();
        for delta in tally.values() {
            match delta.cmp(&0) {
                std::cmp::Ordering::Greater => churn.added += *delta as usize,
                std::cmp::Ordering::Less => churn.removed += delta.unsigned_abs() as usize,
                std::cmp::Ordering::Equal => {}
            }
        }
        churn
    }

    /// Attribute one write's churn to `ingot`. Callers pass the ingot the
    /// writing anvil is forging, so parallel anvils never cross-credit.
    pub fn record_churn(ingot: &str, churn: Churn) {
        if churn.added == 0 && churn.removed == 0 {
            return;
        }
        let mut c = cell();
        let e = c.churn.entry(ingot.to_string()).or_default();
        e.added += churn.added;
        e.removed += churn.removed;
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
            churn: c.churn.clone(),
            ledger: c.ledger.clone(),
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

    /// One `model  role  N tok  $cost` line per ledger row, heaviest
    /// first, or `None` when the run made no calls (item 35). Judge, duel
    /// and founder spend each get their own line, which is the point: a
    /// single session total hides which call site is expensive.
    pub fn ledger_lines(stats: &RunStats) -> Option<Vec<String>> {
        let rows = stats.ledger.rows();
        if rows.is_empty() {
            return None;
        }
        let model_w = rows.iter().map(|r| r.model.len()).max().unwrap_or(0);
        let role_w = rows.iter().map(|r| r.role.label().len()).max().unwrap_or(0);
        Some(
            rows.iter()
                .map(|r| {
                    format!(
                        "{:model_w$}  {:role_w$}  {:>9} tok  {}",
                        r.model,
                        r.role.label(),
                        r.usage.total_tokens,
                        super::pricing::format_cost(&r.usage),
                    )
                })
                .collect(),
        )
    }

    /// `churn: +412/-87 (i2 +300/-12, i1 +112/-75)`, or `None` when the
    /// run wrote nothing. The run total leads because that is the number
    /// an operator reads first; per-ingot detail follows, heaviest first,
    /// so a runaway rewriter is named without scanning the whole list.
    pub fn churn_line(stats: &RunStats) -> Option<String> {
        if stats.churn.is_empty() {
            return None;
        }
        let (added, removed) = stats
            .churn
            .values()
            .fold((0usize, 0usize), |(a, r), c| (a + c.added, r + c.removed));

        let mut per: Vec<(&String, &Churn)> = stats.churn.iter().collect();
        per.sort_by(|a, b| {
            (b.1.added + b.1.removed)
                .cmp(&(a.1.added + a.1.removed))
                .then_with(|| a.0.cmp(b.0))
        });
        let detail: Vec<String> = per
            .into_iter()
            .map(|(id, c)| format!("{id} +{}/-{}", c.added, c.removed))
            .collect();

        // One ingot's detail would just repeat the total.
        if detail.len() == 1 {
            return Some(format!("churn: +{added}/-{removed}"));
        }
        Some(format!("churn: +{added}/-{removed} ({})", detail.join(", ")))
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
                churn: BTreeMap::new(),
                ..Default::default()
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

        /// Item 88: an appended block is pure addition; a rewritten line
        /// is one of each; an unchanged file is no churn at all.
        #[test]
        fn line_churn_counts_added_and_removed_lines() {
            assert_eq!(line_churn("a\nb\n", "a\nb\n"), Churn::default());
            assert_eq!(
                line_churn("a\nb\n", "a\nb\nc\nd\n"),
                Churn { added: 2, removed: 0 }
            );
            assert_eq!(
                line_churn("a\nb\nc\n", "a\n"),
                Churn { added: 0, removed: 2 }
            );
            assert_eq!(
                line_churn("a\nold\nc\n", "a\nnew\nc\n"),
                Churn { added: 1, removed: 1 }
            );
            // A new file: every line is added.
            assert_eq!(line_churn("", "x\ny\n"), Churn { added: 2, removed: 0 });
        }

        /// Item 35: one row per (model, role), heaviest first, so judge and
        /// duel spend read separately from smithing.
        #[test]
        fn ledger_lines_split_spend_per_model_and_role_heaviest_first() {
            let mut stats = RunStats::default();
            let call = |model: &str, role: super::super::Role, cost: f64, tok: u64| {
                super::super::Usage {
                    prompt_tokens: tok,
                    total_tokens: tok,
                    cost: Some(cost),
                    model: Some(model.to_string()),
                    role: Some(role),
                    ..Default::default()
                }
            };
            stats.ledger.fold(&call("base/model", super::super::Role::Smith, 1.0, 900));
            stats.ledger.fold(&call("base/model", super::super::Role::Smith, 0.5, 100));
            stats.ledger.fold(&call("judge/model", super::super::Role::Judge, 0.25, 40));

            let lines = ledger_lines(&stats).expect("calls were made");
            assert_eq!(lines.len(), 2, "one line per (model, role) pair");
            assert!(lines[0].contains("smith"), "got {:?}", lines[0]);
            assert!(lines[0].contains("1000 tok"), "same-key calls fold: {:?}", lines[0]);
            assert!(lines[0].contains("$1.5000"), "got {:?}", lines[0]);
            assert!(lines[1].contains("judge"), "judge spend gets its own row: {:?}", lines[1]);
            // Columns line up: "base/model" pads out to "judge/model"'s width,
            // so both rows put the role at the same offset.
            assert!(lines[0].starts_with("base/model  "), "padded: {:?}", lines[0]);
            // Both rows put the role at the same offset: the model field is
            // padded to the widest id, then two spaces. Checked by offset
            // rather than by `find`, since "judge" also occurs inside
            // "judge/model".
            let role_col = "judge/model".len() + 2;
            assert!(lines[0][role_col..].starts_with("smith"), "role column: {:?}", lines[0]);
            assert!(lines[1][role_col..].starts_with("judge "), "role column: {:?}", lines[1]);
        }

        #[test]
        fn ledger_lines_report_nothing_for_a_run_that_made_no_calls() {
            assert!(ledger_lines(&RunStats::default()).is_none());
        }

        /// An estimated leg taints the row, so the readout never claims the
        /// provider quoted a price it did not.
        #[test]
        fn an_estimated_call_marks_its_ledger_row() {
            let mut stats = RunStats::default();
            stats.ledger.fold(&super::super::Usage {
                total_tokens: 10,
                cost: Some(0.01),
                estimated: true,
                model: Some("base/model".into()),
                role: Some(super::super::Role::Smith),
                ..Default::default()
            });
            let lines = ledger_lines(&stats).unwrap();
            assert!(lines[0].contains("~$0.0100 (est)"), "got {:?}", lines[0]);
        }

        #[test]
        fn churn_line_leads_with_the_run_total_then_heaviest_ingot() {
            let mut stats = RunStats::default();
            stats.churn.insert("i1".into(), Churn { added: 112, removed: 75 });
            stats.churn.insert("i2".into(), Churn { added: 300, removed: 12 });
            assert_eq!(
                churn_line(&stats).unwrap(),
                "churn: +412/-87 (i2 +300/-12, i1 +112/-75)"
            );
        }

        #[test]
        fn churn_line_drops_redundant_detail_and_reports_nothing_for_a_clean_run() {
            assert_eq!(churn_line(&RunStats::default()), None);
            let mut one = RunStats::default();
            one.churn.insert("i1".into(), Churn { added: 9, removed: 1 });
            assert_eq!(churn_line(&one).unwrap(), "churn: +9/-1");
        }

        #[test]
        fn record_churn_ignores_a_no_op_write() {
            let before = snapshot();
            record_churn("i-noop", Churn::default());
            assert!(!snapshot().churn.contains_key("i-noop"));
            assert_eq!(before.churn.get("i-noop"), None);
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

#[cfg(test)]
mod cancellation_tests {
    use super::{CancelReason, Cancellation};

    /// Item 56: the first reason wins. A steer arriving after a ctrl-C
    /// must not relabel an operator abort as a redirect.
    #[test]
    fn the_first_raised_reason_wins() {
        let c = Cancellation::default();
        assert!(!c.is_raised());
        c.raise(CancelReason::UserAbort);
        c.raise(CancelReason::SteerInterrupt);
        assert!(c.is_raised());
        assert_eq!(c.reason(), CancelReason::UserAbort);
    }

    #[test]
    fn a_steer_interrupt_reads_differently_from_an_abort() {
        let c = Cancellation::default();
        c.raise(CancelReason::SteerInterrupt);
        assert_eq!(c.reason(), CancelReason::SteerInterrupt);
        assert_eq!(c.reason().label(), "interrupted to steer");
        assert_eq!(CancelReason::UserAbort.label(), "cancelled by operator");
    }

    /// Clones share both the flag and the reason, so the handler that
    /// raises it and the loop that reads it agree.
    #[test]
    fn clones_share_the_flag_and_the_reason() {
        let c = Cancellation::default();
        let handle = c.clone();
        handle.raise(CancelReason::SteerInterrupt);
        assert!(c.is_raised());
        assert_eq!(c.reason(), CancelReason::SteerInterrupt);
        // The bare flag is the same one every existing caller holds.
        assert!(c.flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// The bridge for the four surfaces that already own a `CancelFlag`.
    #[test]
    fn an_adopted_flag_is_the_same_flag() {
        let flag: super::CancelFlag = Default::default();
        let c = Cancellation::from_flag(flag.clone());
        c.raise(CancelReason::UserAbort);
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst));
    }
}
