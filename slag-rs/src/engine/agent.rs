//! agent — the agentic loop, the smith brain.
//!
//! Drives provider turns and tool dispatch until the model calls `finish`,
//! stops emitting tool calls, or the turn budget runs out. Tool batches run
//! in reader/writer segments (hermes pattern #6): consecutive read-only
//! calls execute concurrently, any writer or unclassified call serializes.
//! Local tool bugs (including panics) become `is_error` tool results;
//! only provider errors propagate — ingot heat handles those retries.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use tokio::task::JoinHandle;

use super::compact::{compact, convo_chars, retain_recent_rounds, summarize};
use super::events::{arg_summary, preview};
use super::tools::ToolBox;
use super::hooks;
use super::transcript::{self, TranscriptWriter};
use super::{
    emit, stats, CancelFlag, CancelReason, Cancellation, ChatMessage, ChatRequest, Effort,
    EngineEvent, EventTx, FinishReason, NormalizedResponse, Provider, RetryPolicy, Role,
    SteerQueue, ToolCall, ToolOutcome, ToolSpec, Usage,
};
use crate::error::SlagError;

const DEFAULT_MAX_TURNS: usize = 40;
const CHAR_BUDGET: usize = 48_000;
const RECENT_TOOL_ROUNDS: usize = 2;
const REVIEW_VIOLATION: &str = "review policy violation:";
/// chars/4 is the estimation rule for text appended since the last usage
/// anchor; token budgets convert to char targets through the same ratio.
pub(crate) const CHARS_PER_TOKEN: usize = 4;
const DEFAULT_TOKEN_BUDGET: usize = CHAR_BUDGET / CHARS_PER_TOKEN;
/// Room reserved out of the model window for its own output.
const OUTPUT_RESERVE_TOKENS: u64 = 16_384;
/// Headroom so compaction fires before the hard window edge.
const COMPACT_BUFFER_TOKENS: u64 = 8_192;
/// Compaction circuit breaker: after this many overflow-shrink cycles the
/// context is declared irrecoverable — a permanent failure, not a heat to
/// keep retrying against.
const MAX_SHRINK_CYCLES: usize = 3;
/// Warn when ingot spend crosses this fraction of the cost cap.
const COST_WARN_FRACTION: f64 = 0.8;
/// Overflow-shrink floor: below this, compaction cannot help — the
/// system prompt, task, and protected tail alone exceed the window.
const CHAR_BUDGET_FLOOR: usize = 16_000;
const TOKEN_BUDGET_FLOOR: usize = CHAR_BUDGET_FLOOR / CHARS_PER_TOKEN;
const PREVIEW_LEN: usize = 80;
const STEER_TAG: &str = "[STEER — operator message, follow it]";
/// Silent-escalation output cap: the first Length truncation retries the
/// same request once with this max_tokens before any continuation nudge.
const RAISED_MAX_TOKENS: u32 = 32_768;
/// Continuation nudges after the silent escalation, before failing.
const MAX_CONTINUE_NUDGES: usize = 3;
const FINISH_CMD_NUDGE: &str = "The task needs a verification command. Call finish with a non-empty cmd string and summary. Use the command that checks the completed work.";
/// CC's proven continuation text: resume directly, no apology, no recap.
const CONTINUE_NUDGE: &str = "Continue your previous response from exactly where it was cut \
off — resume directly, no apology, no recap, do not repeat earlier output.";
/// Output room kept in reserve when shrinking max_tokens to fit a parsed
/// "input + output > limit" overflow.
const OUTPUT_SHRINK_BUFFER: u64 = 1_024;
/// Below this much output room, shrinking max_tokens cannot help — the
/// input itself must be compacted.
const MIN_SHRUNK_OUTPUT_TOKENS: u64 = 512;
/// Files re-read and re-injected after a summarization compaction.
const REINJECT_FILES: usize = 5;
/// Per-file char cap for post-compaction re-injection.
const REINJECT_FILE_CAP: usize = 20_000;
/// Fraction of the token budget the whole re-injection may occupy (the
/// divisor): budget/4 = 25%.
const REINJECT_BUDGET_DIV: usize = 4;

/// Per-file char cap for post-compaction re-injection, sized to the live
/// (possibly halved) token budget: the total re-injection stays within a
/// quarter of the budget instead of a fixed 100k chars that could
/// overflow the very window summarization was shrinking toward. The
/// fixed per-file ceiling still applies on large windows.
fn reinject_file_cap(token_budget: usize) -> usize {
    (token_budget * CHARS_PER_TOKEN / REINJECT_BUDGET_DIV / REINJECT_FILES).min(REINJECT_FILE_CAP)
}

/// Cross-session ingot spend accumulator. One smith invocation is one
/// session, but an ingot burns through many sessions (heats, transient
/// retries); sharing this between them makes `SLAG_MAX_COST_INGOT` cap
/// the ingot, not each session from a fresh $0.
pub type SpendAccum = Arc<std::sync::Mutex<f64>>;

/// `SLAG_CHAR_BUDGET` (in chars, converted at chars/4) still overrides
/// everything so smaller-context models can be forced to compact early;
/// window-derived budgets only apply when it is unset.
fn env_token_budget() -> Option<usize> {
    parse_char_budget(std::env::var("SLAG_CHAR_BUDGET").ok())
}

fn parse_char_budget(raw: Option<String>) -> Option<usize> {
    raw.and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .map(|v| (v / CHARS_PER_TOKEN).max(1))
}

/// Context budget derived from a model window: window minus output reserve
/// minus compaction headroom, floored where compaction stops helping.
fn budget_for_window(window_tokens: u64) -> usize {
    let usable = window_tokens.saturating_sub(OUTPUT_RESERVE_TOKENS + COMPACT_BUFFER_TOKENS);
    (usable as usize).max(TOKEN_BUDGET_FLOOR)
}

/// Usage anchor for token estimation: the provider's reported
/// `prompt_tokens` covers everything sent on the last call exactly; only
/// messages appended since are estimated at chars/4. Anchors replace each
/// other — summing per-turn usage would double-count the shared history.
#[derive(Clone, Copy)]
struct TokenAnchor {
    /// `prompt_tokens` of the last response — exact for `messages[..sent]`.
    tokens: u64,
    /// `messages.len()` at the time of that call.
    sent: usize,
}

/// Estimated prompt tokens for the next call: anchored count plus chars/4
/// of everything appended since; pure chars/4 before any anchor exists.
fn estimate_tokens(messages: &[ChatMessage], anchor: Option<TokenAnchor>) -> u64 {
    match anchor {
        Some(a) if a.sent <= messages.len() => {
            a.tokens + (convo_chars(&messages[a.sent..]) / CHARS_PER_TOKEN) as u64
        }
        _ => (convo_chars(messages) / CHARS_PER_TOKEN) as u64,
    }
}

/// Context fill as a percentage of the compaction trigger. The budget is
/// already net of the output reserve and compaction headroom, so 100 here
/// means "the next turn compacts", not "the window is full". Saturates:
/// an over-budget turn reports 100 rather than wrapping past it.
fn context_pct(used_tokens: u64, budget_tokens: usize) -> u8 {
    if budget_tokens == 0 {
        return 100;
    }
    ((used_tokens * 100) / budget_tokens as u64).min(100) as u8
}

/// Token-driven compaction: when the estimate exceeds the budget, prune
/// enough chars to cover the overshoot (at chars/4). Any prune invalidates
/// the anchor — pruned history no longer matches its counted tokens.
fn compact_to_tokens(
    messages: &mut Vec<ChatMessage>,
    token_budget: usize,
    anchor: &mut Option<TokenAnchor>,
) -> bool {
    let est = estimate_tokens(messages, *anchor);
    if est <= token_budget as u64 {
        return false;
    }
    let over_chars = (est as usize - token_budget) * CHARS_PER_TOKEN;
    let target = convo_chars(messages).saturating_sub(over_chars);
    let changed = compact(messages, target);
    if changed {
        *anchor = None;
    }
    changed
}

/// Provider rejection caused by the request exceeding the model's context
/// window (OpenRouter surfaces these as 400s with varying phrasings).
pub(crate) fn is_context_overflow(e: &SlagError) -> bool {
    if matches!(e, SlagError::ToolValidation(_)) {
        return false;
    }
    let s = e.to_string().to_lowercase();
    [
        "context length",
        "context_length",
        "context window",
        "context limit",
        "maximum context",
        "too many tokens",
        "input is too long",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

/// Parse "A + B > C" token counts from a context-overflow 400 body
/// (Anthropic phrasing: "input length and max_tokens exceed context
/// limit: 195017 + 21333 > 204698"). Returns (input, output, limit).
pub(crate) fn parse_overflow_tokens(msg: &str) -> Option<(u64, u64, u64)> {
    let b = msg.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            if let Some(triple) = parse_overflow_triple(b, i) {
                return Some(triple);
            }
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    None
}

fn parse_overflow_triple(b: &[u8], mut i: usize) -> Option<(u64, u64, u64)> {
    fn take_num(b: &[u8], i: &mut usize) -> Option<u64> {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == start {
            return None;
        }
        std::str::from_utf8(&b[start..*i]).ok()?.parse().ok()
    }
    fn skip_ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    }
    let input = take_num(b, &mut i)?;
    skip_ws(b, &mut i);
    if b.get(i) != Some(&b'+') {
        return None;
    }
    i += 1;
    skip_ws(b, &mut i);
    let output = take_num(b, &mut i)?;
    skip_ws(b, &mut i);
    if b.get(i) != Some(&b'>') {
        return None;
    }
    i += 1;
    skip_ws(b, &mut i);
    let limit = take_num(b, &mut i)?;
    Some((input, output, limit))
}

/// max_tokens that fits a parsed "input + output > limit" overflow, or
/// None when the message carries no numbers or the room left after the
/// input is too small to be worth an output-only retry.
fn shrunk_output_cap(e: &SlagError) -> Option<u32> {
    let (input, _output, limit) = parse_overflow_tokens(&e.to_string())?;
    let room = limit.saturating_sub(input).saturating_sub(OUTPUT_SHRINK_BUFFER);
    (room >= MIN_SHRUNK_OUTPUT_TOKENS).then_some(room.min(u32::MAX as u64) as u32)
}

/// Recover only complete JSON tool payloads that match a declared schema.
fn content_tool_calls(content: &str, specs: &[ToolSpec], turn: usize) -> Option<Vec<ToolCall>> {
    use serde_json::Value;
    let value: Value = serde_json::from_str(content.trim()).ok()?;
    let object = value.as_object()?;
    let payloads: Vec<&Value> = if object.len() == 1 && object.contains_key("tool_calls") {
        object.get("tool_calls")?.as_array()?.iter().collect()
    } else {
        vec![&value]
    };
    if payloads.is_empty() {
        return None;
    }
    payloads.into_iter().enumerate().map(|(index, payload)| {
        let obj = payload.as_object()?;
        let named = if obj.contains_key("function") {
            if obj.keys().any(|k| !["function", "id", "type"].contains(&k.as_str())) {
                return None;
            }
            payload.get("function")
        } else if obj.contains_key("name") && obj.contains_key("arguments") {
            Some(payload)
        } else {
            None
        };
        let (spec, args) = if let Some(named) = named {
            let named = named.as_object()?;
            if named.len() != 2 {
                return None;
            }
            let name = named.get("name")?.as_str()?;
            let spec = specs.iter().find(|s| s.name == name)?;
            let raw = named.get("arguments")?;
            let args = if let Some(text) = raw.as_str() {
                serde_json::from_str(text).ok()?
            } else {
                raw.clone()
            };
            if !tool_args_match(&args, &spec.parameters) {
                return None;
            }
            (spec, args)
        } else {
            let mut matches = specs.iter().filter(|s| tool_args_match(payload, &s.parameters));
            let spec = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            (spec, payload.clone())
        };
        Some(ToolCall {
            id: format!("content_{turn}_{index}"),
            name: spec.name.clone(),
            arguments: args.to_string(),
        })
    }).collect()
}

fn tool_args_match(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    if schema.get("enum").and_then(|v| v.as_array()).is_some_and(|v| !v.contains(value)) {
        return false;
    }
    if ["anyOf", "oneOf", "allOf", "$ref", "not"].iter().any(|key| schema.get(key).is_some()) {
        return false;
    }
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            let Some(object) = value.as_object() else { return false };
            let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else { return false };
            if object.is_empty() || object.iter().any(|(key, val)| {
                properties.get(key).is_none_or(|property| !tool_args_match(val, property))
            }) {
                return false;
            }
            schema.get("required").and_then(|r| r.as_array()).is_none_or(|required| {
                required.iter().all(|key| key.as_str().is_some_and(|key| object.contains_key(key)))
            })
        }
        Some("string") => value.is_string(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        Some("array") => value.as_array().is_some_and(|items| {
            schema.get("items").is_some_and(|item| items.iter().all(|v| tool_args_match(v, item)))
        }),
        _ => false,
    }
}

/// One smith session: a provider, a toolbox, a model, and a turn budget.
pub struct ForgeAgent {
    provider: Arc<dyn Provider>,
    toolbox: ToolBox,
    model: String,
    effort: Option<Effort>,
    max_turns: usize,
    /// Context budget in tokens (window minus reserves, or the env/default
    /// fallback). Compaction is driven by usage-anchored token estimates.
    token_budget: usize,
    /// Dollar ceiling for one ingot (all its sessions); `None` = uncapped.
    cost_cap: Option<f64>,
    /// Dollar ceiling for the whole run; `None` = uncapped. Checked in the
    /// turn loop so an in-flight session stops near the cap instead of
    /// spending until the forge scheduler's between-batches gate fires.
    run_cap: Option<f64>,
    /// Ingot spend shared across this ingot's sessions (see `SpendAccum`).
    ingot_spend: SpendAccum,
    events: Option<EventTx>,
    steer: Option<SteerQueue>,
    cancel: Option<Cancellation>,
    /// Root the transcript journal resolves under (default: the run's
    /// working directory, where `logs/` lives). Tests point it at a
    /// tempdir.
    transcript_root: std::path::PathBuf,
    /// Only the explicit crash-resume path (forge scheduler →
    /// `resume_session`) may replay an open transcript. A fresh strike
    /// always begins fresh: a stale open transcript from a previous job
    /// at the same (ingot, heat) must never hijack a new heat's
    /// conversation.
    resume: bool,
    /// Withhold the `finish` tool, forcing the turn to end with text.
    no_finish: bool,
    require_verification: bool,
    no_tools: bool,
    read_only_tools: bool,
    require_inspection: bool,
    require_runs: bool,
    coordinator_root: Option<std::path::PathBuf>,
    recent_tool_rounds: usize,
    /// Ledger attribution for every call this session makes. `Smith` by
    /// default; the plan-model factory flips it so plan spend separates
    /// from base spend even when both route to the same model id.
    role: Role,
    /// A harness's hooks (`crate::harness`), fired around each tool call
    /// before the operator's own. `None` fires only the operator's.
    harness_hooks: Option<Arc<hooks::HookSnapshot>>,
    /// The only tools this session may see and call. `None` keeps the
    /// usual set.
    tool_allowlist: Option<Vec<String>>,
    /// Files, by path from the root, the session must read before it may
    /// end on text. Empty asks for nothing.
    required_reads: Vec<String>,
}

impl ForgeAgent {
    pub fn new(provider: Arc<dyn Provider>, toolbox: ToolBox, model: impl Into<String>) -> Self {
        Self {
            provider,
            toolbox,
            model: model.into(),
            effort: None,
            max_turns: DEFAULT_MAX_TURNS,
            token_budget: env_token_budget().unwrap_or(DEFAULT_TOKEN_BUDGET),
            cost_cap: crate::config::ingot_cost_cap(),
            run_cap: crate::config::run_cost_cap(),
            ingot_spend: SpendAccum::default(),
            events: None,
            steer: None,
            cancel: None,
            transcript_root: std::path::PathBuf::from("."),
            resume: false,
            no_finish: false,
            require_verification: false,
            no_tools: false,
            read_only_tools: false,
            require_inspection: false,
            require_runs: false,
            coordinator_root: None,
            recent_tool_rounds: RECENT_TOOL_ROUNDS,
            role: Role::Smith,
            harness_hooks: None,
            tool_allowlist: None,
            required_reads: Vec::new(),
        }
    }

    /// Keep the last `rounds` tool rounds verbatim (default 2). A session
    /// that reads many files to compare them needs them all in view.
    pub fn with_recent_rounds(mut self, rounds: usize) -> Self {
        self.recent_tool_rounds = rounds.max(1);
        self
    }

    /// Hold the session's final answer until it has read these files.
    /// A model that claims to have read evidence it never opened gets the
    /// list back, as long as turns remain.
    pub fn with_required_reads(mut self, paths: Vec<String>) -> Self {
        self.required_reads = paths;
        self
    }

    /// Fire a harness's hooks around every tool call.
    pub fn with_harness_hooks(mut self, snap: Option<Arc<hooks::HookSnapshot>>) -> Self {
        self.harness_hooks = snap;
        self
    }

    /// Offer and accept only these tools.
    pub fn with_tool_allowlist(mut self, tools: &[&str]) -> Self {
        self.tool_allowlist = Some(tools.iter().map(|t| t.to_string()).collect());
        self
    }

    /// Withhold the `finish` tool.
    ///
    /// A pass whose output IS a document must end its turn with that
    /// document as text, because the loop returns the text verbatim when no
    /// tool was called. Offered a `finish` tool, models take it -- reading,
    /// then finishing, never writing the document at all -- and the caller
    /// receives a sentence about a blueprint where the blueprint should be.
    /// Removing the tool makes that outcome impossible rather than merely
    /// discouraged by a prompt line the model is free to ignore.
    pub fn without_finish(mut self) -> Self {
        self.no_finish = true;
        self
    }

    pub fn with_required_verification(mut self) -> Self {
        self.require_verification = true;
        self
    }

    pub fn with_read_only_tools(mut self) -> Self {
        self.read_only_tools = true;
        self.no_finish = true;
        self
    }

    pub fn with_no_tools(mut self) -> Self {
        self.no_tools = true;
        self
    }

    pub fn with_required_inspection(mut self) -> Self {
        self.require_inspection = true;
        self
    }

    pub fn with_required_runs(mut self) -> Self {
        self.require_runs = true;
        self
    }

    pub fn with_recent_tool_rounds(mut self, rounds: usize) -> Self {
        self.recent_tool_rounds = rounds.max(1);
        self
    }

    pub fn with_coordinator_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        let root = root.into();
        self.coordinator_root = Some(root.canonicalize().unwrap_or(root));
        self
    }

    /// Attribute this session's spend to a different ledger row.
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    /// Opt in to replaying an open transcript at this (ingot, heat).
    /// Only the crash-resume path sets this; everything else begins fresh.
    pub fn with_resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    /// Override where `logs/transcripts/` resolves (tests).
    pub fn with_transcript_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.transcript_root = root.into();
        self
    }

    #[cfg(test)]
    fn with_char_budget(mut self, chars: usize) -> Self {
        self.token_budget = (chars / CHARS_PER_TOKEN).max(1);
        self.recent_tool_rounds = usize::MAX;
        self
    }

    /// Cap the default budget to a smaller model window after reserves.
    /// `None` keeps the default; `SLAG_CHAR_BUDGET` overrides both.
    pub fn with_context_window(mut self, window_tokens: Option<u64>) -> Self {
        if env_token_budget().is_none() {
            if let Some(window) = window_tokens {
                self.token_budget = budget_for_window(window).min(DEFAULT_TOKEN_BUDGET);
            }
        }
        self
    }

    pub fn with_effort(mut self, effort: Option<Effort>) -> Self {
        self.effort = effort;
        self
    }

    /// Override the per-ingot spend cap (tests; `new` reads config/env).
    pub fn with_cost_cap(mut self, cap: Option<f64>) -> Self {
        self.cost_cap = cap;
        self
    }

    /// Override the run-wide spend cap (tests; `new` reads config/env).
    pub fn with_run_cap(mut self, cap: Option<f64>) -> Self {
        self.run_cap = cap;
        self
    }

    /// Share one spend accumulator across every session of an ingot.
    pub fn with_ingot_spend(mut self, acc: SpendAccum) -> Self {
        self.ingot_spend = acc;
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns.max(1);
        self
    }

    pub fn with_events(mut self, tx: EventTx) -> Self {
        self.events = Some(tx);
        self
    }

    pub fn with_steer(mut self, q: SteerQueue) -> Self {
        self.steer = Some(q);
        self
    }

    /// Adopt a bare flag — the bridge for the surfaces that own one and
    /// cannot say why they raised it. Reads as `UserAbort`, the
    /// conservative default: an unexplained stop is reported as a failure.
    pub fn with_cancel(self, f: CancelFlag) -> Self {
        self.with_cancellation(Cancellation::from_flag(f))
    }

    /// Take the flag *and* its reason (item 56), so a steer interrupt can
    /// skip the error path a ctrl-C takes.
    pub fn with_cancellation(mut self, c: Cancellation) -> Self {
        // The toolbox needs the same flag, or a ten-minute bash proof keeps
        // running after the operator interrupted the session.
        self.toolbox = self.toolbox.clone().with_cancel(c.flag.clone());
        self.cancel = Some(c);
        self
    }

    /// Run the loop to completion. Returns the final summary text.
    pub async fn run(&self, system: String, task: String) -> Result<String, SlagError> {
        // Item 67: inside a forge attempt (`transcript::scope`), every
        // message is journaled to logs/transcripts/<ingot>-h<heat>.jsonl.
        // The `end` entry closes the transcript on any exit; only a
        // process death leaves it open — and resumable.
        let tw = TranscriptWriter::for_current(&self.transcript_root);
        // Steers drained into the conversation die with it if the provider
        // errors: re-queue them so the ingot's next heat re-delivers them.
        let mut applied_steers: Vec<String> = Vec::new();
        let result = self.run_inner(system, task, &mut applied_steers, tw.as_ref()).await;
        if let Some(w) = &tw {
            w.end(result.is_ok());
        }
        if result.is_err() {
            self.requeue_steers(&applied_steers);
        }
        result
    }

    async fn run_inner(
        &self,
        system: String,
        task: String,
        applied_steers: &mut Vec<String>,
        tw: Option<&TranscriptWriter>,
    ) -> Result<String, SlagError> {
        let required_runs: Vec<String> = if self.require_runs {
            task.lines().next().and_then(|line| line.strip_prefix("REQUIRED RUN COMMANDS: "))
                .map(serde_json::from_str).transpose()
                .map_err(|e| SlagError::SmithFailed(format!("invalid required run commands: {e}")))?
                .unwrap_or_default()
        } else { Vec::new() };
        // Resume-or-begin: on the explicit crash-resume path an open
        // transcript at this same (ingot, heat) replays its recorded
        // conversation and the loop continues where it died; otherwise the
        // attempt starts fresh and records from the top (begin truncates
        // any stale transcript a dead prior job left behind). A resumed
        // session gets a fresh turn budget — the recorded context is what
        // matters.
        let recorded = if self.resume {
            tw.and_then(|w| transcript::resumable_messages(w.path()))
        } else {
            None
        };
        let mut messages = match recorded {
            Some(recorded) => {
                emit(
                    &self.events,
                    EngineEvent::Warning {
                        message: format!(
                            "resumed transcript ({} messages) — continuing the interrupted session",
                            recorded.len()
                        ),
                    },
                );
                recorded
            }
            None => {
                let fresh = vec![ChatMessage::system(system), ChatMessage::user(task)];
                if let Some(w) = tw {
                    w.begin(&fresh);
                }
                fresh
            }
        };
        let mut token_budget = self.token_budget;
        let mut anchor: Option<TokenAnchor> = None;
        let mut budget_warned = false;
        // Output-cap recovery ladder state (item 51): silent escalation
        // first, then counted continuation nudges.
        let mut out_tokens: Option<u32> = None;
        let mut escalated = false;
        let mut nudges = 0usize;
        let mut tool_validation_errors = 0usize;
        let mut inspected = false;
        let mut read: Vec<String> = Vec::new();
        let require_verification = self.require_verification && !self.no_finish && !self.no_tools;
        let mut finish_cmd_nudged = false;
        let mut finish_repair_pending = false;
        let mut inspections = crate::pipeline::warden::InspectionLog::default();

        for turn in 1..=self.max_turns.saturating_add(1) {
            if turn > self.max_turns && !finish_repair_pending {
                break;
            }
            finish_repair_pending = false;
            self.check_cancel()?;
            // A parallel anvil or the operator may have rewritten a file
            // this session is tracking; say so before the model reasons
            // over a stale view of it.
            let externals = self.toolbox.external_changes().await;
            let steered = applied_steers.len();
            self.apply_steers(&mut messages, applied_steers);
            if !externals.is_empty() {
                self.inject_reminders(&mut messages, &externals);
                record_rewrite(tw, &messages);
            }
            if applied_steers.len() > steered {
                // Steers rewrite an existing tool result in place: the
                // append-only journal can only stay true via a redump.
                record_rewrite(tw, &messages);
            }
            if retain_recent_rounds(&mut messages, self.recent_tool_rounds) {
                anchor = None;
                record_rewrite(tw, &messages);
            }
            if compact_to_tokens(&mut messages, token_budget, &mut anchor) {
                record_rewrite(tw, &messages);
            }
            let used = estimate_tokens(&messages, anchor);
            emit(
                &self.events,
                EngineEvent::ContextGauge {
                    pct: context_pct(used, token_budget),
                    used_tokens: used,
                    budget_tokens: token_budget,
                },
            );
            emit(&self.events, EngineEvent::TurnStart { turn });
            emit(&self.events, EngineEvent::ModelCall { model: self.model.clone() });

            // Item 55: watermark the committed history before the attempt.
            // Every retry that re-sends the *same* request rebuilds from
            // here, so a failed attempt's partial assistant/tool messages
            // cannot ride along. Retries that deliberately carry the
            // partial (the continuation nudge) append past the mark.
            let committed = messages.len();
            let mut resp = match self
                .chat_shrinking(&mut messages, true, &mut token_budget, &mut anchor, &mut out_tokens, tw)
                .await
            {
                Ok(resp) => resp,
                Err(SlagError::ToolValidation(message)) => {
                    tool_validation_errors += 1;
                    if tool_validation_errors >= 3 {
                        return Err(SlagError::ToolValidation(message));
                    }
                    messages.push(ChatMessage::user(format!(
                        "Tool error from provider: {message}\nRetry with a corrected declared tool call. \
                         Supply all required arguments with the declared types."
                    )));
                    record_msg(tw, messages.last().unwrap());
                    continue;
                }
                Err(e) => {
                    emit(&self.events, EngineEvent::Error { message: e.to_string() });
                    return Err(e);
                }
            };
            tool_validation_errors = 0;
            // Re-anchor on the provider's own count: prompt_tokens is exact
            // for everything just sent; chars/4 only ever covers the delta.
            if resp.usage.prompt_tokens > 0 {
                anchor = Some(TokenAnchor {
                    tokens: resp.usage.prompt_tokens,
                    sent: messages.len(),
                });
            }
            // With a router in front, the requested id says nothing about
            // what actually did the work. Report the swap when it happens.
            if let Some(routed) = resp.model.as_deref().filter(|m| *m != self.model) {
                emit(
                    &self.events,
                    EngineEvent::ModelRouted {
                        requested: self.model.clone(),
                        routed: routed.to_string(),
                    },
                );
            }
            emit(&self.events, EngineEvent::Tokens { usage: resp.usage.clone() });
            self.track_spend(&resp.usage);

            if resp.tool_calls.is_empty() && resp.finish_reason != FinishReason::Length {
                let specs = self.request(Vec::new(), true, out_tokens).tools;
                if let Some(calls) = content_tool_calls(&resp.content, &specs, turn) {
                    emit(&self.events, EngineEvent::Warning {
                        message: "recovered declared tool call from assistant JSON".into(),
                    });
                    resp.tool_calls = calls;
                    resp.content.clear();
                }
            }
            if resp.tool_calls.is_empty() {
                if resp.finish_reason == FinishReason::Length {
                    if !escalated {
                        // Silent escalation: discard the partial and retry
                        // the same request with a raised output cap — no
                        // transcript noise when the cap alone was the issue.
                        escalated = true;
                        out_tokens = Some(out_tokens.unwrap_or(0).max(RAISED_MAX_TOKENS));
                        messages.truncate(committed);
                        continue;
                    }
                    if nudges < MAX_CONTINUE_NUDGES {
                        // Truncated even at the raised cap: keep the partial
                        // and nudge a direct resume. This one appends past
                        // the watermark on purpose — the model needs its own
                        // partial to continue from.
                        nudges += 1;
                        messages.push(ChatMessage::assistant(resp.content, None));
                        messages.push(ChatMessage::user(CONTINUE_NUDGE));
                        record_msg(tw, &messages[messages.len() - 2]);
                        record_msg(tw, &messages[messages.len() - 1]);
                        continue;
                    }
                    let e = SlagError::SmithFailed(format!(
                        "output truncated at the token cap despite a raised max_tokens retry \
and {MAX_CONTINUE_NUDGES} continuation nudges"
                    ));
                    emit(&self.events, EngineEvent::Error { message: e.to_string() });
                    return Err(e);
                }
                if self.require_inspection && !inspected {
                    messages.push(ChatMessage::user(
                        "Read the artifact files or run their checks before returning a verdict. Use an inspection tool now.",
                    ));
                    record_msg(tw, messages.last().unwrap());
                    continue;
                }
                if require_verification && !has_verification_command(&resp.content) && !finish_cmd_nudged {
                    finish_cmd_nudged = true;
                    finish_repair_pending = true;
                    messages.push(ChatMessage::assistant(resp.content, None));
                    messages.push(ChatMessage::user(FINISH_CMD_NUDGE));
                    record_msg(tw, &messages[messages.len() - 2]);
                    record_msg(tw, &messages[messages.len() - 1]);
                    emit(&self.events, EngineEvent::Warning {
                        message: "finish omitted the verification command; requesting one corrected finish call".into(),
                    });
                    continue;
                }
                let unread: Vec<&str> = self
                    .required_reads
                    .iter()
                    .filter(|p| !read.iter().any(|r| r == *p || r.ends_with(&format!("/{p}"))))
                    .map(String::as_str)
                    .collect();
                if !unread.is_empty() && turn < self.max_turns {
                    messages.push(ChatMessage::assistant(resp.content.clone(), None));
                    messages.push(ChatMessage::user(format!(
                        "You have not read these files, and your answer must rest on them. Read them with \
                         read_file, then answer again: {}",
                        unread.join(", ")
                    )));
                    record_msg(tw, &messages[messages.len() - 2]);
                    record_msg(tw, messages.last().unwrap());
                    continue;
                }
                let report = self.ground_review_report(resp.content, &inspections, &required_runs);
                emit(&self.events, EngineEvent::Finish { summary: report.clone() });
                return Ok(report);
            }

            // Budget gate sits before the assistant tool_calls message is
            // pushed, so a capped session never leaves dangling ids. The
            // ingot spend is cumulative across this ingot's sessions.
            let spent = *self.ingot_spend.lock().unwrap();
            if let Some(cap) = self.cost_cap {
                if spent >= cap {
                    let e = SlagError::SmithFailed(format!(
                        "ingot budget exhausted (${spent:.4} of ${cap:.4} cap)"
                    ));
                    emit(&self.events, EngineEvent::Error { message: e.to_string() });
                    return Err(e);
                }
                if !budget_warned && spent >= cap * COST_WARN_FRACTION {
                    budget_warned = true;
                    emit(
                        &self.events,
                        EngineEvent::Warning {
                            message: format!(
                                "ingot spend ${spent:.4} passed 80% of ${cap:.4} cap"
                            ),
                        },
                    );
                }
            }
            // Run-wide gate: without it a session keeps forging (and
            // spending) long after the run total crossed the cap, since
            // the forge scheduler only checks between anvil batches.
            if let Some(cap) = self.run_cap {
                let run_spent = crate::config::run_spend_dollars();
                if run_spent >= cap {
                    let e = SlagError::RunBudgetExhausted { spent: run_spent, cap };
                    emit(&self.events, EngineEvent::Error { message: e.to_string() });
                    return Err(e);
                }
            }

            let calls = resp.tool_calls;
            messages.push(
                ChatMessage::assistant(resp.content.clone(), Some(calls.clone()))
                    .with_reasoning_details(resp.reasoning_details.clone()),
            );
            record_msg(tw, messages.last().unwrap());

            let mut outcomes = self.dispatch_batch(&calls).await;
            if require_verification && !finish_cmd_nudged && !has_verification_command(&resp.content) {
                for (call, outcome) in calls.iter().zip(&mut outcomes) {
                    if call.name == "finish" && !outcome.is_error && !has_verification_command(&outcome.output) {
                        outcome.output = FINISH_CMD_NUDGE.into();
                        outcome.is_error = true;
                        finish_repair_pending = true;
                    }
                }
                if finish_repair_pending {
                    finish_cmd_nudged = true;
                    emit(&self.events, EngineEvent::Warning {
                        message: "finish omitted the verification command; requesting one corrected finish call".into(),
                    });
                }
            }

            // Every tool_call_id gets a tool result, errors included — the
            // API never sees a dangling id (hermes pattern #7).
            let mut finish_summary: Option<String> = None;
            for (call, outcome) in calls.iter().zip(&outcomes) {
                inspected |= !outcome.is_error && matches!(call.name.as_str(), "read_file" | "bash");
                if call.name == "read_file" && !outcome.is_error {
                    if let Some(path) = serde_json::from_str::<serde_json::Value>(&call.arguments)
                        .ok()
                        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(|p| p.trim_start_matches("./").to_string()))
                    {
                        read.push(path);
                    }
                }
                let mut content = if outcome.is_error {
                    format!("ERROR: {}", outcome.output)
                } else {
                    outcome.output.clone()
                };
                if self.read_only_tools {
                    if let Some(reference) = inspections.record(call, outcome) {
                        content.push_str(&format!("\n\n{reference}"));
                    }
                }
                messages.push(ChatMessage::tool_result(&call.id, content));
                record_msg(tw, messages.last().unwrap());
                if call.name == "finish" && !outcome.is_error && finish_summary.is_none() {
                    finish_summary = Some(outcome.output.clone());
                }
            }

            if self.read_only_tools {
                if let Some(outcome) = outcomes.iter().find(|o| o.output.starts_with(REVIEW_VIOLATION)) {
                    return Err(SlagError::SmithFailed(outcome.output.clone()));
                }
            }

            if let Some(summary) = finish_summary {
                // Keep the same-turn assistant text: Plan mode delivers the
                // plan as final text before calling finish, and forge-mode
                // contracts (e.g. a CMD: line) may live there too.
                let result = if resp.content.trim().is_empty() {
                    summary
                } else {
                    format!("{}\n\n{}", resp.content, summary)
                };
                emit(&self.events, EngineEvent::Finish { summary: result.clone() });
                return Ok(result);
            }

            // Drain again right after tool execution: a steer typed during
            // a long bash lands before the next model call.
            let steered = applied_steers.len();
            self.apply_steers(&mut messages, applied_steers);
            if applied_steers.len() > steered {
                record_rewrite(tw, &messages);
            }
        }

        // Turn budget exhausted: one final no-tools call for a summary.
        // A cancel raised during the last batch stops here — the batch's
        // tool results are already backfilled, so the transcript is whole.
        self.check_cancel()?;
        if self.require_inspection && !inspected {
            return Err(SlagError::SmithFailed("review ended without a successful artifact inspection".into()));
        }
        messages.push(ChatMessage::user(if require_verification {
            "No more tool budget. Summarize the completed work and include CMD: <verification command> on its own line. Return text without tool calls."
        } else {
            "no more tool budget — summarize what was done"
        }));
        record_msg(tw, messages.last().unwrap());
        if retain_recent_rounds(&mut messages, self.recent_tool_rounds) {
            anchor = None;
            record_rewrite(tw, &messages);
        }
        if compact_to_tokens(&mut messages, token_budget, &mut anchor) {
            record_rewrite(tw, &messages);
        }
        let resp = match self
            .chat_shrinking(&mut messages, false, &mut token_budget, &mut anchor, &mut out_tokens, tw)
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                emit(&self.events, EngineEvent::Error { message: e.to_string() });
                return Err(e);
            }
        };
        emit(&self.events, EngineEvent::Tokens { usage: resp.usage.clone() });
        self.track_spend(&resp.usage);
        let content = if require_verification {
            resp.content.lines().map(|line| match line.trim_start().split_once(':') {
                Some((key, command)) if key.eq_ignore_ascii_case("cmd") => format!("CMD:{command}"),
                _ => line.to_string(),
            }).collect::<Vec<_>>().join("\n")
        } else { resp.content };
        let report = self.ground_review_report(content, &inspections, &required_runs);
        emit(&self.events, EngineEvent::Finish { summary: report.clone() });
        Ok(report)
    }

    fn ground_review_report(
        &self,
        raw: String,
        inspections: &crate::pipeline::warden::InspectionLog,
        required_runs: &[String],
    ) -> String {
        let has_verdict = raw.lines().any(|line| line.trim().trim_start_matches(['#', '*', '-', ' '])
            .split_once(':').is_some_and(|(key, _)| key.trim_matches(['*', '_', '`']).trim().eq_ignore_ascii_case("VERDICT")));
        if !self.read_only_tools || !self.require_inspection && !has_verdict { return raw; }
        if crate::pipeline::warden::parse_verdict(&raw).gap.contains("no readable verdict") {
            if let Some(root) = &self.coordinator_root {
                static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let logs = root.join("logs");
                if std::fs::create_dir_all(&logs).is_ok() {
                    let name = format!("warden-raw-{}-{}-{}.log",
                        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(), std::process::id(),
                        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
                    let _ = std::fs::write(logs.join(name), raw.as_bytes());
                }
            }
        }
        let verdict = crate::pipeline::warden::parse_grounded_verdict(&raw, inspections, self.require_runs, required_runs);
        let outcome = if verdict.fulfilled { "pass" } else if verdict.is_unverified() { "unverified" } else { "fail" };
        format!("VERDICT: {outcome}\nGAP: {}\nEVIDENCE: {}", verdict.gap, verdict.evidence)
    }

    /// Fold one response's cost into the shared ingot accumulator and the
    /// run-wide spend accumulator (see `spend_for`).
    fn track_spend(&self, usage: &Usage) {
        let c = spend_for(usage);
        if c > 0.0 {
            *self.ingot_spend.lock().unwrap() += c;
            crate::config::add_run_spend(c);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.is_raised())
    }

    /// Hard interrupt: checked at each turn boundary, before the model call.
    ///
    /// Item 56: the run ends either way, but only an abort reports an
    /// error. A steer interrupt is the operator redirecting live work, so
    /// it narrates the reason and leaves the error path alone.
    fn check_cancel(&self) -> Result<(), SlagError> {
        if self.is_cancelled() {
            let e = SlagError::Cancelled;
            let reason =
                self.cancel.as_ref().map(Cancellation::reason).unwrap_or_default();
            match reason {
                CancelReason::SteerInterrupt => {
                    emit(&self.events, EngineEvent::Narrate { text: reason.label().into() });
                }
                CancelReason::UserAbort => {
                    emit(&self.events, EngineEvent::Error { message: e.to_string() });
                }
            }
            return Err(e);
        }
        Ok(())
    }

    /// Chat with context-overflow recovery, cheapest remedy first: (1) a
    /// parsed "input + output > limit" 400 retries once with a shrunk
    /// max_tokens — no history lost when only the output budget overflowed;
    /// (2) halve the token budget, stub-prune/drop-rounds compact, retry —
    /// until the floor is reached or pruning stops making progress; (3) one
    /// LLM summarization pass replaces the head with a 9-section summary
    /// before giving up.
    async fn chat_shrinking(
        &self,
        messages: &mut Vec<ChatMessage>,
        with_tools: bool,
        token_budget: &mut usize,
        anchor: &mut Option<TokenAnchor>,
        out_tokens: &mut Option<u32>,
        tw: Option<&TranscriptWriter>,
    ) -> Result<NormalizedResponse, SlagError> {
        let mut shrink_cycles = 0usize;
        let mut output_shrunk = false;
        let mut summarized = false;
        let mut attempts = 0usize;
        loop {
            // Item 87: each provider.chat() is timed; attempts past the
            // first are retry overhead (overflow shrink/summarize
            // re-sends), split out in the assay durations line.
            attempts += 1;
            let started = Instant::now();
            let outcome = self
                .provider
                .chat(self.request(messages.clone(), with_tools, *out_tokens))
                .await;
            stats::record_api(started.elapsed(), attempts > 1);
            match outcome {
                Ok(resp) => return Ok(resp),
                Err(e) if is_context_overflow(&e) => {
                    // Output-only overflow: shrink max_tokens once before
                    // any compaction — cheaper when the input still fits.
                    if !output_shrunk {
                        output_shrunk = true;
                        if let Some(cap) = shrunk_output_cap(&e) {
                            if out_tokens.is_none_or(|t| cap < t) {
                                *out_tokens = Some(cap);
                                emit(
                                    &self.events,
                                    EngineEvent::Error {
                                        message: format!(
                                            "context overflow — shrunk max_tokens to {cap}, retrying"
                                        ),
                                    },
                                );
                                continue;
                            }
                        }
                    }
                    // Circuit breaker: shrink cycles that keep overflowing
                    // mean the window can never fit — fail permanently
                    // instead of grinding halvings forever.
                    shrink_cycles += 1;
                    if shrink_cycles > MAX_SHRINK_CYCLES {
                        return Err(SlagError::SmithFailed(format!(
                            "context irrecoverable after {MAX_SHRINK_CYCLES} compactions: {e}"
                        )));
                    }
                    let can_prune = *token_budget > TOKEN_BUDGET_FLOOR;
                    *token_budget = (*token_budget / 2).max(TOKEN_BUDGET_FLOOR);
                    if can_prune && compact_to_tokens(messages, *token_budget, anchor) {
                        record_rewrite(tw, messages);
                        emit(
                            &self.events,
                            EngineEvent::Error {
                                message: format!(
                                    "context overflow — compacted to {token_budget} tokens, retrying"
                                ),
                            },
                        );
                        continue;
                    }
                    // Stage two: pruning cannot reach the budget — one LLM
                    // summarization pass before giving up.
                    if summarized {
                        return Err(e);
                    }
                    summarized = true;
                    let snapshots = self
                        .toolbox
                        .recent_file_snapshots(REINJECT_FILES, reinject_file_cap(*token_budget));
                    // The summarizer's calls carry nearly the whole
                    // conversation head — the largest requests of the
                    // session. Route them through the spend wrapper so
                    // they count against the ingot/run accumulators and
                    // stop at the run cap like every other call.
                    let tracked = SpendTracked::new(self.provider.as_ref(), self.ingot_spend.clone())
                        .with_run_cap(self.run_cap);
                    match summarize(&tracked, &self.model, messages, &snapshots).await {
                        Ok(true) => {
                            *anchor = None;
                            record_rewrite(tw, messages);
                            emit(
                                &self.events,
                                EngineEvent::Error {
                                    message: "context overflow — summarized history, retrying"
                                        .into(),
                                },
                            );
                        }
                        _ => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Inject queued steering messages (hermes steer-into-tool-result
    /// pattern): each steer appends to the last tool result so the model
    /// reads it with the tool output; before any tool result exists it
    /// rides as a user message. Order is preserved. Every applied steer is
    /// recorded so a failed run can put it back in the queue.
    fn apply_steers(&self, messages: &mut Vec<ChatMessage>, applied: &mut Vec<String>) {
        for text in self.drain_steers() {
            emit(&self.events, EngineEvent::Steer { text: text.clone() });
            self.inject_reminders(messages, &[format!("{STEER_TAG}\n{text}")]);
            applied.push(text);
        }
    }

    /// Attach reminder bodies to the conversation using the same carrier
    /// rule as steers: onto the last tool result when there is one, so the
    /// model reads them with the tool output, otherwise as a user message.
    /// `append_reminder` coalesces, so several notes share one envelope.
    fn inject_reminders(&self, messages: &mut Vec<ChatMessage>, notes: &[String]) {
        for note in notes {
            match messages.iter_mut().rev().find(|m| m.role == "tool") {
                Some(m) => crate::engine::prompt::append_reminder(&mut m.content, note),
                None => match messages.last_mut().filter(|m| {
                    m.role == "user"
                        && m.content.trim_end().ends_with(crate::engine::prompt::REMINDER_CLOSE)
                }) {
                    Some(m) => crate::engine::prompt::append_reminder(&mut m.content, note),
                    None => messages
                        .push(ChatMessage::user(crate::engine::prompt::wrap_reminder(note))),
                },
            }
        }
    }

    /// Put steers consumed by a failed run back at the front of the queue,
    /// ahead of anything queued since, so the retry heat re-delivers them.
    fn requeue_steers(&self, applied: &[String]) {
        if applied.is_empty() {
            return;
        }
        if let Some(q) = &self.steer {
            let mut q = q.lock().unwrap();
            let mut restored: Vec<String> = applied.to_vec();
            restored.append(&mut *q);
            *q = restored;
        }
    }

    /// Take all queued steers. Synchronous on purpose: the std Mutex guard
    /// is taken and dropped inside this fn, never held across an await.
    fn drain_steers(&self) -> Vec<String> {
        match &self.steer {
            Some(q) => std::mem::take(&mut *q.lock().unwrap()),
            None => Vec::new(),
        }
    }

    fn request(
        &self,
        messages: Vec<ChatMessage>,
        with_tools: bool,
        max_tokens: Option<u32>,
    ) -> ChatRequest {
        ChatRequest {
            model: self.model.clone(),
            messages,
            tools: if with_tools && !self.no_tools {
                let mut specs = ToolBox::all_specs();
                if self.no_finish {
                    specs.retain(|t| t.name != "finish");
                }
                if self.require_verification {
                    if let Some(finish) = specs.iter_mut().find(|t| t.name == "finish") {
                        finish.parameters["required"] = serde_json::json!(["summary", "cmd"]);
                        finish.parameters["properties"]["cmd"]["minLength"] = serde_json::json!(1);
                    }
                }
                if self.read_only_tools {
                    specs.retain(|t| review_tool(&t.name));
                }
                if let Some(allow) = &self.tool_allowlist {
                    specs.retain(|t| allow.contains(&t.name));
                }
                specs
            } else {
                Vec::new()
            },
            effort: self.effort,
            max_tokens,
            role: self.role,
            // A forge strike is the work itself: full provider budget.
            retry: RetryPolicy::full(),
        }
    }

    /// Execute a tool batch in reader/writer segments, preserving model
    /// order in the returned outcomes.
    async fn dispatch_batch(&self, calls: &[ToolCall]) -> Vec<ToolOutcome> {
        let batch_started = Instant::now();
        let mut outcomes: Vec<Option<ToolOutcome>> = calls.iter().map(|_| None).collect();

        for segment in plan_segments(calls) {
            // Cancel raised mid-batch: skip the remaining segments. Their
            // slots backfill below so every tool_call_id still gets a
            // result — the transcript never ends on a dangling id.
            if self.is_cancelled() {
                break;
            }
            // Spawn the whole segment first (concurrent for readers), then
            // await in model order.
            let handles: Vec<(usize, JoinHandle<ToolOutcome>)> = segment
                .iter()
                .map(|&i| {
                    emit(
                        &self.events,
                        EngineEvent::ToolCallStart {
                            name: calls[i].name.clone(),
                            preview: preview(&arg_summary(&calls[i].name, &calls[i].arguments), PREVIEW_LEN),
                        },
                    );
                    (i, self.spawn_call(&calls[i]))
                })
                .collect();

            for (i, handle) in handles {
                let dispatched = std::time::Instant::now();
                let outcome = match handle.await {
                    Ok(outcome) => outcome,
                    // A panic inside a tool is a local bug, not a loop killer.
                    Err(e) => ToolOutcome {
                        output: format!("tool dispatch panicked: {e}"),
                        is_error: true,
                    },
                };
                emit(
                    &self.events,
                    EngineEvent::ToolResult {
                        name: calls[i].name.clone(),
                        ok: !outcome.is_error,
                        preview: preview(&outcome.output, PREVIEW_LEN),
                        // Measured on the full output: the preview is
                        // truncated, and the point of the counts is to say
                        // how much the collapsed line is hiding.
                        lines: outcome.output.lines().count(),
                        bytes: outcome.output.len(),
                        ms: dispatched.elapsed().as_millis() as u64,
                        unchanged: !outcome.is_error
                            && super::tools::left_unchanged(&calls[i].name, &outcome.output),
                    },
                );
                // Item 89: failed tool calls tally per tool with a coarse
                // error class for the assay report.
                if outcome.is_error {
                    stats::record_tool_error(&calls[i].name, &outcome.output);
                }
                outcomes[i] = Some(outcome);
            }
        }
        stats::record_tools(batch_started.elapsed());

        let skipped = if self.is_cancelled() {
            "cancelled: interrupted before this tool ran"
        } else {
            "internal: tool call was never dispatched"
        };
        outcomes
            .into_iter()
            .map(|o| {
                o.unwrap_or(ToolOutcome {
                    output: skipped.into(),
                    is_error: true,
                })
            })
            .collect()
    }

    fn spawn_call(&self, call: &ToolCall) -> JoinHandle<ToolOutcome> {
        // Task-locals do not cross tokio::spawn: capture the attempt
        // context here (the agent's task) and bind it onto the dispatched
        // toolbox so write/edit checkpointing (item 68) still knows which
        // attempt it belongs to.
        let toolbox = self.toolbox.clone().with_attempt(transcript::current());
        let call = call.clone();
        let steer = self.steer.clone();
        let events = self.events.clone();
        let root = self.coordinator_root.clone();
        let read_only = self.read_only_tools;
        let no_tools = self.no_tools;
        let harness = self.harness_hooks.clone();
        let refused = self.tool_allowlist.as_ref().is_some_and(|allow| !allow.contains(&call.name));
        tokio::spawn(async move {
            if no_tools {
                return ToolOutcome { output: "tools are unavailable in this session".into(), is_error: true };
            }
            if refused {
                return ToolOutcome {
                    output: format!("{} is unavailable in this session", call.name),
                    is_error: true,
                };
            }
            if read_only && !review_tool(&call.name) {
                return ToolOutcome {
                    output: format!("{REVIEW_VIOLATION} {} is unavailable during review", call.name),
                    is_error: true,
                };
            }
            dispatch_hooked(toolbox, call, steer, events, root.as_deref(), read_only, harness.as_deref()).await
        })
    }
}

/// One tool call, wrapped in its lifecycle hooks (items 69, 75).
///
/// PreToolUse runs first: exit 2 refuses the call outright, and a
/// structured reply can rewrite the arguments before dispatch. The result
/// then goes back through PostToolUse or ToolError, whose output is
/// appended to what the smith reads. With no hooks configured this is a
/// snapshot read and two empty selects — the cost the common case pays.
///
/// A harness's hooks fire first on the way in, so the operator's own
/// guards judge the call a harness may have rewritten, and last on the
/// way out.
async fn dispatch_hooked(
    toolbox: ToolBox,
    mut call: ToolCall,
    steer: Option<SteerQueue>,
    events: Option<EventTx>,
    coordinator_root: Option<&std::path::Path>,
    read_only_tools: bool,
    harness: Option<&hooks::HookSnapshot>,
) -> ToolOutcome {
    if read_only_tools {
        return guarded_dispatch(&toolbox, &call, coordinator_root, true).await;
    }
    let mut harness_context = Vec::new();
    if let Some(snap) = harness {
        let pre = hooks::fire_with(
            snap,
            hooks::HookEvent::PreTool,
            hooks::HookPayload::new(hooks::HookEvent::PreTool).with_tool(&call.name, &call.arguments),
            &call.name,
            &call.arguments,
            steer.as_ref(),
            events.as_ref(),
        )
        .await;
        if let Some(reason) = pre.blocked {
            return ToolOutcome { output: format!("blocked by hook: {reason}"), is_error: true };
        }
        if let Some(rewritten) = pre.rewritten_arguments() {
            call.arguments = rewritten;
        }
        harness_context = pre.context;
    }
    let pre = hooks::fire(
        hooks::HookEvent::PreTool,
        hooks::HookPayload::new(hooks::HookEvent::PreTool).with_tool(&call.name, &call.arguments),
        &call.name,
        &call.arguments,
        steer.as_ref(),
        events.as_ref(),
    )
    .await;

    if let Some(reason) = pre.blocked {
        // The refusal is the tool result: the smith reads why and picks
        // another move, which is the whole point of a guard hook.
        return ToolOutcome {
            output: format!("blocked by hook: {reason}"),
            is_error: true,
        };
    }
    if let Some(rewritten) = pre.rewritten_arguments() {
        call.arguments = rewritten;
    }

    let mut outcome = guarded_dispatch(&toolbox, &call, coordinator_root, read_only_tools).await;

    let event = if outcome.is_error {
        hooks::HookEvent::ToolError
    } else {
        hooks::HookEvent::PostTool
    };
    let post = hooks::fire(
        event,
        hooks::HookPayload::new(event)
            .with_tool(&call.name, &call.arguments)
            .with_output(&outcome.output),
        &call.name,
        &call.arguments,
        steer.as_ref(),
        events.as_ref(),
    )
    .await;

    // A post hook's exit 2 cannot un-run the call, so its reason joins the
    // result as context the smith must answer for.
    let mut appended = harness_context;
    appended.extend(pre.context);
    appended.extend(post.context);
    if let Some(reason) = post.blocked {
        appended.push(format!("hook objected: {reason}"));
        outcome.is_error = true;
    }
    if let Some(snap) = harness {
        let post = hooks::fire_with(
            snap,
            event,
            hooks::HookPayload::new(event)
                .with_tool(&call.name, &call.arguments)
                .with_output(&outcome.output),
            &call.name,
            &call.arguments,
            steer.as_ref(),
            events.as_ref(),
        )
        .await;
        appended.extend(post.context);
        if let Some(reason) = post.blocked {
            appended.push(format!("hook objected: {reason}"));
            outcome.is_error = true;
        }
    }
    if !appended.is_empty() {
        outcome.output = format!("{}\n\n{}", outcome.output, appended.join("\n"));
    }
    outcome
}

fn review_tool(name: &str) -> bool {
    matches!(name, "read_file" | "bash")
}

fn coordinator_path(path: &std::path::Path) -> bool {
    matches!(path.components().next(), Some(std::path::Component::Normal(name)) if name == "logs")
        || matches!(path.to_str(), Some("PLAN.md" | "PROGRESS.md" | "BLUEPRINT.md" | "BAR.md" | "PRD.md" | "AGENTS.md"))
}

pub(crate) fn coordinator_command_error(command: &str) -> Option<&'static str> {
    let words: Vec<_> = command.split(|c: char| c.is_whitespace() || ";|&()".contains(c))
        .map(|s| s.trim_matches(['\'', '"']))
        .collect();
    if words.iter().any(|w| matches!(*w, "restore" | "reset" | "checkout" | "clean"))
        && words.contains(&"git")
    {
        return Some("slag owns coordinator files; do not restore, reset, checkout, or clean the worktree");
    }
    let call = ToolCall {
        id: String::new(),
        name: "bash".into(),
        arguments: serde_json::json!({"command": command}).to_string(),
    };
    let reads_only = ToolBox::path_access(&call).is_some_and(|(_, writer)| !writer);
    if !reads_only && words.iter().any(|w| {
        let path = std::path::Path::new(w.trim_matches(['>', '<']));
        path.components().any(|c| matches!(c, std::path::Component::Normal(name) if matches!(name.to_str(), Some("PLAN.md" | "PROGRESS.md" | "BLUEPRINT.md" | "BAR.md" | "PRD.md" | "AGENTS.md" | "logs"))))
    }) {
        return Some("slag owns coordinator files and logs; inspect them with read_file and change only artifact files");
    }
    None
}

async fn guarded_dispatch(
    toolbox: &ToolBox,
    call: &ToolCall,
    root: Option<&std::path::Path>,
    read_only: bool,
) -> ToolOutcome {
    let refused = |message: String| ToolOutcome { output: message, is_error: true };
    if read_only && !review_tool(&call.name) {
        return refused(format!("{REVIEW_VIOLATION} {} is unavailable during review", call.name));
    }
    if read_only && call.name == "bash" {
        return review_shell(call, root).await;
    }
    let Some(root) = root else {
        return toolbox.dispatch(call).await;
    };
    if let Some((path, true)) = ToolBox::path_access(call) {
        let path = root.join(path);
        let resolved = path.canonicalize().unwrap_or(path);
        let relative = resolved.strip_prefix(root).unwrap_or(&resolved);
        if coordinator_path(relative) {
            return refused("slag owns this coordinator file; change only artifact files".into());
        }
    }
    if call.name == "bash" {
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments) {
            if args.get("run_in_background").and_then(serde_json::Value::as_bool) == Some(true) {
                return refused("run commands in the foreground so slag can protect coordinator files".into());
            }
            if args.get("command").and_then(serde_json::Value::as_str)
                .is_some_and(|command| declared_verification(root, command))
            {
                return review_shell(call, Some(root)).await;
            }
            if let Some(reason) = args.get("command").and_then(serde_json::Value::as_str)
                .and_then(coordinator_command_error)
            {
                return refused(reason.into());
            }
        }
    }
    if ToolBox::path_access(call).is_some_and(|(_, writer)| !writer) || call.name == "finish" {
        return toolbox.dispatch(call).await;
    }
    let _lock = crate::crucible::CRUCIBLE_LOCK.lock().await;
    let guard = match crate::pipeline::CoordinatorGuard::capture(root) {
        Ok(guard) => guard,
        Err(e) => return refused(format!("cannot protect coordinator files: {e}")),
    };
    let mut result = toolbox.dispatch(call).await;
    match guard.restore() {
        Ok(changed) if !changed.is_empty() => {
            result.is_error = true;
            result.output.push_str(&format!(
                "\nslag restored its coordinator files: {}. Change only artifact files.",
                changed.join(", ")
            ));
        }
        Err(e) => return refused(format!("cannot restore coordinator files: {e}")),
        _ => {}
    }
    result
}

fn declared_verification(root: &std::path::Path, command: &str) -> bool {
    let Ok(bar) = std::fs::read_to_string(root.join("BAR.md")) else { return false };
    let Some(required) = bar.lines().find_map(|line| line.strip_prefix("REQUIRED RUN COMMANDS: ")
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())) else { return false };
    let Ok(actual) = shell_words::split(command) else { return false };
    required.iter().any(|required| shell_words::split(required).ok()
        .is_some_and(|words| !words.is_empty() && actual.starts_with(&words)))
}

async fn review_shell(call: &ToolCall, root: Option<&std::path::Path>) -> ToolOutcome {
    use std::process::Stdio;
    let violation = |message: String| ToolOutcome {
        output: format!("{REVIEW_VIOLATION} {message}"), is_error: true,
    };
    let Some(root) = root else { return violation("review has no workspace root".into()); };
    let args: serde_json::Value = match serde_json::from_str(&call.arguments) {
        Ok(args) => args,
        Err(e) => return violation(format!("invalid shell arguments: {e}")),
    };
    let Some(command) = args.get("command").and_then(serde_json::Value::as_str) else {
        return violation("review shell needs a command".into());
    };
    if args.get("run_in_background").and_then(serde_json::Value::as_bool) == Some(true) {
        return violation("background commands are forbidden during review".into());
    }
    if let Some(reason) = review_command_error(command) { return violation(reason.into()); }
    let sandbox = match super::sandbox::Sandbox::read_only() {
        Ok(sandbox) => sandbox,
        Err(e) => return violation(e.to_string()),
    };
    let timeout = args.get("timeout").and_then(serde_json::Value::as_u64).unwrap_or(20).clamp(1, 30);
    let mut cmd = sandbox.wrap("/bin/sh");
    cmd.args(["-c", command]).current_dir(root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("NODE_DISABLE_COMPILE_CACHE", "1")
        .env("PIP_NO_INDEX", "1")
        .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return violation(format!("cannot start read-only shell: {e}")),
    };
    #[cfg(unix)]
    let pgid = child.id();
    let output = match tokio::time::timeout(std::time::Duration::from_secs(timeout), child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return violation(format!("read-only shell failed: {e}")),
        Err(_) => {
            #[cfg(unix)]
            if let Some(pgid) = pgid {
                let _ = tokio::process::Command::new("/bin/kill").args(["-KILL", &format!("-{pgid}")]).status().await;
            }
            return violation(format!("read-only shell exceeded {timeout}s"));
        }
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    text.push_str(&stderr);
    let lower = if output.status.success() { stderr.to_ascii_lowercase() } else { text.to_ascii_lowercase() };
    if ["operation not permitted", "permission denied", "read-only file system", "sandbox_apply:", "no usable temporary directory"]
        .iter().any(|marker| lower.contains(marker))
    {
        return violation(format!("the read-only shell refused access\n{}", preview(&text, 2_000)));
    }
    if text.chars().count() > 30_000 { text = text.chars().take(30_000).collect(); }
    if !output.status.success() {
        text.push_str(&format!("\n(exit {})", output.status.code().unwrap_or(-1)));
    }
    ToolOutcome { output: text, is_error: !output.status.success() }
}

pub(crate) fn review_command_error(command: &str) -> Option<&'static str> {
    if let Some(reason) = coordinator_command_error(command) { return Some(reason); }
    let words: Vec<_> = command.split(|c: char| c.is_whitespace() || ";|&()".contains(c))
        .map(|word| word.trim_matches(['\'', '"']))
        .collect();
    let installer = words.iter().any(|word| {
        let name = std::path::Path::new(word).file_name().and_then(|s| s.to_str()).unwrap_or(word);
        matches!(name, "pip" | "pip3" | "npm" | "pnpm" | "yarn" | "bun" | "uv" | "brew" | "apt" | "apt-get" | "cargo" | "gem")
    });
    if words.iter().any(|word| {
        matches!(*word, "touch" | "rm" | "mkdir" | "rmdir" | "mv" | "cp" | "chmod" | "chown" | "truncate" | "tee" | "dd" | "patch")
    }) {
        return Some("filesystem mutation commands are forbidden during review");
    }
    if words.contains(&"npx") || words.contains(&"uv") && words.contains(&"run")
        || installer && words.iter().any(|word| {
        matches!(*word, "install" | "add" | "sync" | "ci" | "update" | "upgrade" | "exec")
    }) {
        return Some("package installs and package resolution are forbidden during review");
    }
    None
}

fn has_verification_command(text: &str) -> bool {
    crate::proof::extract_cmd(text).is_some_and(|cmd| !cmd.is_empty())
}

/// Journal one appended message (no-op outside a forge attempt).
fn record_msg(tw: Option<&TranscriptWriter>, msg: &ChatMessage) {
    if let Some(w) = tw {
        w.record(msg);
    }
}

/// Journal a wholesale history rewrite (compaction, summarization, steer
/// injection): boundary + redump, so resume loads only the live view.
fn record_rewrite(tw: Option<&TranscriptWriter>, messages: &[ChatMessage]) {
    if let Some(w) = tw {
        w.boundary_and_redump(messages);
    }
}

/// Plan tool calls into segments preserving model order: consecutive
/// reader-only calls share a segment (run concurrently); any writer or
/// unclassified call (bash/grep/finish) gets a segment of its own.
fn plan_segments(calls: &[ToolCall]) -> Vec<Vec<usize>> {
    let mut segments: Vec<Vec<usize>> = Vec::new();
    let mut readers: Vec<usize> = Vec::new();

    for (i, call) in calls.iter().enumerate() {
        match ToolBox::path_access(call) {
            Some((_, false)) => readers.push(i),
            _ => {
                if !readers.is_empty() {
                    segments.push(std::mem::take(&mut readers));
                }
                segments.push(vec![i]);
            }
        }
    }
    if !readers.is_empty() {
        segments.push(readers);
    }
    segments
}

/// Dollars one response cost. A reported `usage.cost` is the truth — an
/// explicit `0.0` (OpenRouter `:free` variants report it on every
/// response) means free, NOT missing, so it must not be overridden by
/// the estimate or free-model runs accrue phantom spend and trip caps.
/// Only an absent cost (proxy setups strip the field) falls back to the
/// token estimate; a negative report is nonsense and treated as absent.
pub(crate) fn spend_for(usage: &Usage) -> f64 {
    usage
        .cost
        .filter(|c| *c >= 0.0)
        .unwrap_or_else(|| estimated_cost(usage))
}

/// Provider wrapper that folds every response's cost into a shared ingot
/// accumulator and the run-wide spend, and refuses to call out once the
/// run cap is spent. The judge/assayer holds no `ForgeAgent` (and so no
/// `track_spend`); without this wrapper its calls bypass both spend caps.
pub struct SpendTracked<P> {
    inner: P,
    accum: SpendAccum,
    run_cap: Option<f64>,
}

impl<P: Provider> SpendTracked<P> {
    pub fn new(inner: P, accum: SpendAccum) -> Self {
        Self { inner, accum, run_cap: crate::config::run_cost_cap() }
    }

    /// Override the run-wide cap (`new` reads config/env; the agent's
    /// summarize wrapper passes its own, test-overridable cap through).
    pub fn with_run_cap(mut self, cap: Option<f64>) -> Self {
        self.run_cap = cap;
        self
    }
}

impl<P: Provider> Provider for SpendTracked<P> {
    fn chat(
        &self,
        req: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>> {
        let accum = self.accum.clone();
        let run_cap = self.run_cap;
        Box::pin(async move {
            if let Some(cap) = run_cap {
                let spent = crate::config::run_spend_dollars();
                if spent >= cap {
                    return Err(SlagError::RunBudgetExhausted { spent, cap });
                }
            }
            let resp = self.inner.chat(req).await?;
            let c = spend_for(&resp.usage);
            if c > 0.0 {
                *accum.lock().unwrap() += c;
                crate::config::add_run_spend(c);
            }
            Ok(resp)
        })
    }

    /// Observability wiring must survive the spend wrapper: forward the
    /// heartbeat sink and cancel flag to the wrapped provider.
    fn set_event_sink(&self, tx: EventTx) {
        self.inner.set_event_sink(tx)
    }

    fn set_cancel_flag(&self, f: CancelFlag) {
        self.inner.set_cancel_flag(f)
    }
}

/// Conservative flat rate for spend estimation when a provider response
/// carries no `usage.cost` (proxy setups strip it). Overridable via
/// SLAG_COST_PER_MTOK; the default errs high so budget caps still bind.
const COST_PER_MTOK_DEFAULT: f64 = 5.0;

/// Estimated dollars for one response: total tokens at the flat rate.
fn estimated_cost(usage: &Usage) -> f64 {
    estimated_cost_at(usage, cost_per_mtok())
}

fn estimated_cost_at(usage: &Usage, rate_per_mtok: f64) -> f64 {
    usage.total_tokens as f64 * rate_per_mtok / 1_000_000.0
}

fn cost_per_mtok() -> f64 {
    mtok_rate_from(std::env::var("SLAG_COST_PER_MTOK").ok().as_deref())
}

/// Parse an override rate; junk, negatives, and non-finite values fall
/// back to the default.
fn mtok_rate_from(v: Option<&str>) -> f64 {
    v.and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|r| r.is_finite() && *r >= 0.0)
        .unwrap_or(COST_PER_MTOK_DEFAULT)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_plan_pass_is_offered_no_finish_tool() {
        // The document-eating bug in one assertion: offered a finish tool,
        // a survey pass reads, finishes, and never writes the document, so
        // the caller gets a sentence about a blueprint. Withholding the
        // tool makes that impossible rather than discouraged.
        let dir = tempfile::tempdir().unwrap();
        let mk = || agent(MockProvider::new(vec![]), dir.path());
        let names = |a: &ForgeAgent| -> Vec<String> {
            a.request(Vec::new(), true, None).tools.iter().map(|t| t.name.clone()).collect()
        };
        assert!(names(&mk()).iter().any(|n| n == "finish"), "a forge pass keeps finish");
        let planner = mk().without_finish();
        assert!(!names(&planner).iter().any(|n| n == "finish"), "a plan pass has none");
        assert!(
            names(&planner).iter().any(|n| n == "read_file"),
            "withholding finish must not disarm the rest of the toolbox"
        );
    }

    use super::*;
    use crate::engine::NormalizedResponse;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    // Only the tests reach past `Cancellation` to the raw atomic now.
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    /// Scripted provider: pops one response per chat call, records requests.
    struct MockProvider {
        script: Mutex<VecDeque<NormalizedResponse>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl MockProvider {
        fn new(script: Vec<NormalizedResponse>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Provider for MockProvider {
        fn chat(
            &self,
            req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            self.requests.lock().unwrap().push(req);
            let resp = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("mock script exhausted");
            Box::pin(async move { Ok(resp) })
        }
    }

    fn tc(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    fn resp_tools(calls: Vec<ToolCall>) -> NormalizedResponse {
        NormalizedResponse {
            model: None,
            content: String::new(),
            tool_calls: calls,
            finish_reason: FinishReason::ToolCalls,
            reasoning: None,
            reasoning_details: None,
            usage: Default::default(),
        }
    }

    fn resp_text(text: &str, finish_reason: FinishReason) -> NormalizedResponse {
        NormalizedResponse {
            model: None,
            content: text.into(),
            tool_calls: vec![],
            finish_reason,
            reasoning: None,
            reasoning_details: None,
            usage: Default::default(),
        }
    }

    fn agent(provider: Arc<MockProvider>, root: &std::path::Path) -> ForgeAgent {
        // Caps pinned off: other tests mutate the cap env vars and the
        // global run-spend accumulator; budget tests opt in explicitly.
        ForgeAgent::new(provider, ToolBox::new(root), "test/model")
            .with_cost_cap(None)
            .with_run_cap(None)
    }

    #[tokio::test]
    async fn an_allowlist_offers_and_accepts_only_its_tools() {
        let dir = tempfile::tempdir().unwrap();
        let editor = agent(MockProvider::new(vec![]), dir.path()).with_tool_allowlist(&["read_file", "write_file"]);
        let names: Vec<String> = editor.request(Vec::new(), true, None).tools.into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["read_file", "write_file"]);
        let outcomes = editor.dispatch_batch(&[tc("b", "bash", serde_json::json!({"command": "touch marker"}))]).await;
        assert!(outcomes[0].is_error && outcomes[0].output.contains("unavailable"), "{:?}", outcomes[0]);
        assert!(!dir.path().join("marker").exists());
    }

    #[tokio::test]
    async fn a_session_can_keep_every_tool_round_in_view() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.md", "b.md", "c.md"] {
            std::fs::write(dir.path().join(name), format!("evidence {name}\n")).unwrap();
        }
        let script = || {
            vec![
                resp_tools(vec![tc("1", "read_file", serde_json::json!({"path": "a.md"}))]),
                resp_tools(vec![tc("2", "read_file", serde_json::json!({"path": "b.md"}))]),
                resp_tools(vec![tc("3", "read_file", serde_json::json!({"path": "c.md"}))]),
                resp_text("done", FinishReason::Stop),
            ]
        };
        let kept = |p: &MockProvider| {
            let last = p.requests().last().unwrap().messages.clone();
            ["a.md", "b.md", "c.md"].iter().filter(|n| last.iter().any(|m| m.role == "tool" && m.content.contains(&format!("evidence {n}")))).count()
        };
        let short = MockProvider::new(script());
        agent(short.clone(), dir.path()).without_finish().run("s".into(), "t".into()).await.unwrap();
        assert_eq!(kept(&short), 2, "the default keeps two rounds verbatim");
        let long = MockProvider::new(script());
        agent(long.clone(), dir.path()).without_finish().with_recent_rounds(usize::MAX).run("s".into(), "t".into()).await.unwrap();
        assert_eq!(kept(&long), 3);
    }

    #[tokio::test]
    async fn an_answer_waits_until_the_required_files_are_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("context/runs/base-1")).unwrap();
        std::fs::write(dir.path().join("context/runs/base-1/trajectory.md"), "turn 1\n").unwrap();
        let provider = MockProvider::new(vec![
            resp_text("KEEP_BASE: I read everything.", FinishReason::Stop),
            resp_tools(vec![tc("r", "read_file", serde_json::json!({"path": "./context/runs/base-1/trajectory.md"}))]),
            resp_text("KEEP_BASE: base-1 already passed.", FinishReason::Stop),
        ]);
        let editor = agent(provider.clone(), dir.path())
            .without_finish()
            .with_required_reads(vec!["context/runs/base-1/trajectory.md".into()]);
        let reply = editor.run("edit".into(), "task".into()).await.unwrap();
        assert_eq!(reply, "KEEP_BASE: base-1 already passed.");
        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        let nudge = &requests[1].messages.last().unwrap().content;
        assert!(nudge.contains("You have not read these files") && nudge.contains("context/runs/base-1/trajectory.md"), "{nudge}");
    }

    #[tokio::test]
    async fn harness_hooks_answer_around_a_tool_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        let snap = hooks::HookSnapshot::from_entries(
            vec![
                ("post_tool".into(), "name=note matcher=read_file cmd='echo harness saw it'".into()),
                ("pre_tool".into(), "name=stop matcher=write_file cmd='echo no writes here >&2; exit 2'".into()),
            ],
            false,
        );
        let a = agent(MockProvider::new(vec![]), dir.path()).with_harness_hooks(Some(Arc::new(snap)));
        let outcomes = a
            .dispatch_batch(&[
                tc("r", "read_file", serde_json::json!({"path": "a.txt"})),
                tc("w", "write_file", serde_json::json!({"path": "b.txt", "content": "y"})),
            ])
            .await;
        assert!(outcomes[0].output.ends_with("harness saw it"), "{:?}", outcomes[0]);
        assert!(outcomes[1].is_error && outcomes[1].output.contains("no writes here"), "{:?}", outcomes[1]);
        assert!(!dir.path().join("b.txt").exists());
    }

    #[tokio::test]
    async fn review_withholds_write_tools_and_refuses_an_unoffered_write() {
        let dir = tempfile::tempdir().unwrap();
        let reviewer = agent(MockProvider::new(vec![]), dir.path()).with_read_only_tools();
        let specs = reviewer.request(Vec::new(), true, None).tools;
        assert!(specs.iter().any(|tool| tool.name == "read_file"));
        assert!(specs.iter().all(|tool| review_tool(&tool.name)));
        let outcomes = reviewer.dispatch_batch(&[tc(
            "w", "write_file", serde_json::json!({"path": "artifact.py", "content": "bad"}),
        )]).await;
        assert!(outcomes[0].is_error);
        assert!(!dir.path().join("artifact.py").exists());
    }

    #[tokio::test]
    async fn readonly_review_mutation_taints_the_session_before_a_later_pass() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("artifact.py"), "VALUE = 42\n").unwrap();
        for mutation in [
            tc("w", "write_file", serde_json::json!({"path": "artifact.py", "content": "wrong"})),
            tc("b", "bash", serde_json::json!({"command": "touch marker"})),
            tc("p", "bash", serde_json::json!({"command": "python3 -m pip install --target /tmp/forbidden-package pytest"})),
            tc("t", "bash", serde_json::json!({"command": "python3 -c 'import tempfile; tempfile.NamedTemporaryFile()'"})),
        ] {
            let provider = MockProvider::new(vec![
                resp_tools(vec![tc("r", "read_file", serde_json::json!({"path": "artifact.py"})), mutation]),
                resp_text("VERDICT: pass", FinishReason::Stop),
            ]);
            let error = agent(provider.clone(), dir.path()).with_coordinator_root(dir.path()).with_read_only_tools()
                .run("review".into(), "check artifact".into()).await.unwrap_err();
            assert!(error.to_string().contains(REVIEW_VIOLATION), "{error}");
            assert_eq!(provider.requests().len(), 1, "tainted review must not ask for another verdict");
            assert_eq!(std::fs::read_to_string(dir.path().join("artifact.py")).unwrap(), "VALUE = 42\n");
            assert!(!dir.path().join("marker").exists());
        }
    }

    #[tokio::test]
    async fn readonly_review_shell_runs_probes_or_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("artifact.py"), "VALUE = 42\n").unwrap();
        let call = tc("b", "bash", serde_json::json!({
            "command": "python3 -c 'import artifact, os; assert artifact.VALUE == 42; assert os.environ[\"NODE_DISABLE_COMPILE_CACHE\"] == \"1\"; print(artifact.VALUE)'",
        }));
        let result = review_shell(&call, Some(dir.path())).await;
        if super::super::sandbox::backend() == super::super::sandbox::Backend::None {
            assert!(result.is_error);
            assert!(result.output.starts_with(REVIEW_VIOLATION));
        } else {
            assert!(!result.is_error, "{}", result.output);
            assert!(result.output.contains("42"));
        }
        assert!(!dir.path().join("__pycache__").exists());
    }

    #[tokio::test]
    async fn direct_builder_checks_are_readonly_but_construction_stays_writable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("BAR.md"), "REQUIRED RUN COMMANDS: [\"python3 -m unittest\"]\n").unwrap();
        std::fs::write(dir.path().join("test_direct.py"),
            "import pathlib, unittest\nclass TestDirect(unittest.TestCase):\n def test_write(self):\n  pathlib.Path('stray-fixture').write_text('bad')\n").unwrap();
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("suite", "bash", serde_json::json!({"command": "python3 -m unittest -v"}))]),
            resp_tools(vec![tc("fixture", "write_file", serde_json::json!({"path": "fixture.txt", "content": "intended"}))]),
            resp_text("construction continues", FinishReason::Stop),
        ]);
        let result = agent(provider.clone(), dir.path()).with_coordinator_root(dir.path())
            .run("system".into(), "task".into()).await.unwrap();
        assert_eq!(result, "construction continues");
        let requests = provider.requests();
        assert!(requests[1].messages.last().unwrap().content.starts_with("ERROR:"));
        assert!(!requests[2].messages.last().unwrap().content.starts_with("ERROR:"));
        assert!(!dir.path().join("stray-fixture").exists());
        assert!(!dir.path().join("__pycache__").exists());
        assert_eq!(std::fs::read_to_string(dir.path().join("fixture.txt")).unwrap(), "intended");
        assert!(!declared_verification(dir.path(), "python3 -c 'print(1)'"));
        assert!(!declared_verification(dir.path(), "python3 -m unittest_extra"));
    }

    #[tokio::test]
    async fn required_review_inspection_rejects_an_unsupported_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("VERDICT: PASS", FinishReason::Stop)]);
        let error = agent(provider.clone(), dir.path())
            .with_read_only_tools().with_required_inspection().with_max_turns(1)
            .run("review".into(), "inspect artifact".into()).await.unwrap_err();
        assert!(error.to_string().contains("without a successful artifact inspection"));
        assert_eq!(provider.requests().len(), 1);

        std::fs::write(dir.path().join("artifact.py"), "print('hello')").unwrap();
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("r", "read_file", serde_json::json!({"path": "artifact.py"}))]),
            resp_text("VERDICT: PASS\nEVIDENCE: [\"r\"]", FinishReason::Stop),
        ]);
        let result = agent(provider, dir.path()).with_read_only_tools().with_required_inspection()
            .run("review".into(), "inspect artifact".into()).await.unwrap();
        let verdict = crate::pipeline::warden::parse_verdict(&result);
        assert!(verdict.fulfilled);
        assert!(verdict.evidence.contains("read_file \"artifact.py\""));
    }

    #[tokio::test]
    async fn invalid_required_run_header_stops_before_a_provider_call() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![]);
        let error = agent(provider.clone(), dir.path()).with_read_only_tools().with_required_runs()
            .run("review".into(), "REQUIRED RUN COMMANDS: [1]\nInspect the artifact.".into())
            .await.unwrap_err();
        assert!(error.to_string().contains("invalid required run commands"));
        assert!(provider.requests().is_empty());
    }

    #[tokio::test]
    async fn unreadable_review_keeps_the_exact_raw_response() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("artifact.py"), "VALUE = 42\n").unwrap();
        let raw = "I cannot choose a verdict.\n  Preserve these spaces.\n";
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("r", "read_file", serde_json::json!({"path": "artifact.py"}))]),
            resp_text(raw, FinishReason::Stop),
        ]);
        let result = agent(provider, dir.path()).with_coordinator_root(dir.path())
            .with_read_only_tools().with_required_inspection().with_required_runs()
            .run("review".into(), "Inspect artifact.py.".into()).await.unwrap();
        assert!(crate::pipeline::warden::parse_verdict(&result).is_unverified());
        let logs: Vec<_> = std::fs::read_dir(dir.path().join("logs")).unwrap()
            .filter_map(Result::ok).map(|entry| entry.path()).collect();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].file_name().unwrap().to_string_lossy().starts_with("warden-raw-"));
        assert_eq!(std::fs::read_to_string(&logs[0]).unwrap(), raw);
    }

    #[test]
    fn review_allows_package_test_scripts_without_package_installation() {
        for command in ["npm test", "npm run test", "pnpm run test", "node --test"] {
            assert!(review_command_error(command).is_none(), "{command}");
        }
        for command in ["npm install", "npm ci", "npx vitest", "uv run pytest"] {
            assert!(review_command_error(command).is_some(), "{command}");
        }
    }

    #[tokio::test]
    async fn review_grounding_survives_history_trimming_and_the_final_budget_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("artifact.py"), "VALUE = 42\n").unwrap();
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("first", "read_file", serde_json::json!({"path": "artifact.py"}))]),
            resp_tools(vec![tc("second", "read_file", serde_json::json!({"path": "artifact.py"}))]),
            resp_text("VERDICT: pass\nEVIDENCE: [\"first\"]", FinishReason::Stop),
        ]);
        let reviewer = agent(provider.clone(), dir.path()).with_read_only_tools()
            .with_required_inspection().with_max_turns(2).with_recent_tool_rounds(1);
        let result = reviewer.run("review".into(), "inspect artifact".into()).await.unwrap();
        assert!(crate::pipeline::warden::parse_verdict(&result).fulfilled);
        assert!(result.contains("[\"first\"] read_file \"artifact.py\""));
        assert_eq!(provider.requests().len(), 3);
        assert!(provider.requests()[2].messages.iter().all(|message| message.tool_call_id.as_deref() != Some("first")));
    }

    #[tokio::test]
    async fn decorated_plan_verdicts_cannot_bypass_grounding() {
        let dir = tempfile::tempdir().unwrap();
        for verdict in ["\tVERDICT: pass", "\u{2003}VERDICT: pass", "#\tVERDICT: pass"] {
            let provider = MockProvider::new(vec![resp_text(
                &format!("{verdict}\nEVIDENCE: fabricated.py"), FinishReason::Stop,
            )]);
            let result = agent(provider, dir.path()).with_read_only_tools()
                .run("plan review".into(), "judge document".into()).await.unwrap();
            assert!(crate::pipeline::warden::parse_verdict(&result).is_unverified());
        }
    }

    #[tokio::test]
    async fn a_no_tools_session_withholds_and_refuses_tools() {
        let dir = tempfile::tempdir().unwrap();
        let classifier = agent(MockProvider::new(vec![]), dir.path()).with_no_tools();
        assert!(classifier.request(Vec::new(), true, None).tools.is_empty());
        let result = classifier.dispatch_batch(&[tc("b", "bash", serde_json::json!({
            "command": "printf bad > artifact.txt",
        }))]).await;
        assert!(result[0].is_error);
        assert!(!dir.path().join("artifact.txt").exists());
    }

    #[tokio::test]
    async fn coordinator_guard_blocks_direct_writes_and_restores_indirect_shell_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("PLAN.md"), "live status").unwrap();
        let smith = agent(MockProvider::new(vec![]), dir.path())
            .with_coordinator_root(dir.path());
        let outcomes = smith.dispatch_batch(&[
            tc("w", "write_file", serde_json::json!({"path": "PLAN.md", "content": "stale status"})),
            tc("b", "bash", serde_json::json!({"command": "name=PLAN; printf stale > \"${name}.md\"; printf artifact > artifact.txt"})),
        ]).await;
        assert!(outcomes.iter().all(|outcome| outcome.is_error));
        assert!(outcomes[0].output.contains("slag owns"));
        assert!(outcomes[1].output.contains("slag restored"));
        assert_eq!(std::fs::read_to_string(dir.path().join("PLAN.md")).unwrap(), "live status");
        assert_eq!(std::fs::read_to_string(dir.path().join("artifact.txt")).unwrap(), "artifact");
    }

    #[tokio::test]
    async fn coordinator_guard_repairs_indirect_shell_changes_to_recovery_logs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs/transcripts")).unwrap();
        let engine = dir.path().join("logs/engine-20261017_120000.jsonl");
        let transcript = dir.path().join("logs/transcripts/i1-h1.jsonl");
        std::fs::write(&engine, "{\"event\":\"turn_start\",\"turn\":1}\n").unwrap();
        std::fs::write(&transcript, "{\"entry\":\"resume_attempt\"}\n").unwrap();
        let smith = agent(MockProvider::new(vec![]), dir.path()).with_coordinator_root(dir.path());
        let command = "name=lo; name=\"${name}gs\"; printf bad > \"$name/engine-20261017_120000.jsonl\"; rm \"$name/transcripts/i1-h1.jsonl\"; printf marker > \"$name/new-marker\"";
        let outcome = smith.dispatch_batch(&[tc("b", "bash", serde_json::json!({"command": command}))]).await;
        assert!(outcome[0].is_error);
        assert!(outcome[0].output.contains("slag restored"));
        assert_eq!(std::fs::read_to_string(engine).unwrap(), "{\"event\":\"turn_start\",\"turn\":1}\n");
        assert_eq!(std::fs::read_to_string(transcript).unwrap(), "{\"entry\":\"resume_attempt\"}\n");
        assert!(!dir.path().join("logs/new-marker").exists());
    }

    #[test]
    fn coordinator_commands_allow_inspection_and_reject_restores_or_log_writes() {
        assert!(coordinator_command_error("git status --porcelain").is_none());
        assert!(coordinator_command_error("cat PLAN.md").is_none());
        assert!(coordinator_command_error("git restore .").is_some());
        assert!(coordinator_command_error("git -C . checkout HEAD -- PLAN.md").is_some());
        assert!(coordinator_command_error("rm -rf logs/").is_some());
        assert!(coordinator_command_error("printf bad > PLAN.md").is_some());
        assert!(coordinator_command_error("python3 -m unittest").is_none());
    }

    #[tokio::test]
    async fn requests_keep_only_two_complete_tool_rounds_and_a_small_evidence_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let mut script = Vec::new();
        for i in 0..6 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("evidence {i}\n").repeat(500)).unwrap();
            script.push(resp_tools(vec![tc(
                &format!("c{i}"), "read_file", serde_json::json!({"path": format!("f{i}.txt")}),
            )]));
        }
        script.push(resp_text("done", FinishReason::Stop));
        let provider = MockProvider::new(script);
        agent(provider.clone(), dir.path()).run("system".into(), "original task".into()).await.unwrap();
        let requests = provider.requests();
        let last = &requests.last().unwrap().messages;
        assert_eq!(last[1].content, "original task");
        assert_eq!(last.len(), 7);
        assert!(last[2].content.contains("Earlier tool evidence"));
        assert_eq!(last[3].tool_calls.as_ref().unwrap()[0].id, "c4");
        assert_eq!(last[5].tool_calls.as_ref().unwrap()[0].id, "c5");
        let full_file_chars: usize = (0..6).map(|i| {
            std::fs::read_to_string(dir.path().join(format!("f{i}.txt"))).unwrap().len()
        }).sum();
        assert!(convo_chars(last) < full_file_chars);
    }

    #[test]
    fn content_tools_require_complete_unambiguous_declared_json() {
        let specs = ToolBox::specs();
        let finish = r#"{"cmd":"python3 -m unittest","summary":"Done"}"#;
        let calls = content_tool_calls(finish, &specs, 1).unwrap();
        assert_eq!(calls[0].name, "finish");
        for text in [
            format!("Done: {finish}"),
            format!("```json\n{finish}\n```"),
            format!("{finish}\nThat is an example."),
            r#"{"cmd":"true","summary":42}"#.into(),
            r#"{"cmd":"true","summary":"Done","extra":true}"#.into(),
            r#"{"pattern":"hello"}"#.into(),
            r#"{"name":"grep","arguments":{"path":"."}}"#.into(),
            r#"{"name":"undeclared","arguments":{"summary":"Done"}}"#.into(),
            r#"{"name":"bash","arguments":{"command":"true","timeout":"10"}}"#.into(),
            r#"{"name":"grep","arguments":{"pattern":"x","output_mode":"invalid"}}"#.into(),
        ] {
            assert!(content_tool_calls(&text, &specs, 1).is_none(), "{text}");
        }
        let mut no_finish = specs.clone();
        no_finish.retain(|s| s.name != "finish");
        assert!(content_tool_calls(finish, &no_finish, 1).is_none());
    }

    #[test]
    fn content_tools_accept_named_calls_and_batches() {
        let text = r#"{"tool_calls":[{"function":{"name":"grep","arguments":"{\"pattern\":\"hello\"}"},"type":"function","id":"x"},{"name":"finish","arguments":{"summary":"Done","cmd":"true"}}]}"#;
        let calls = content_tool_calls(text, &ToolBox::specs(), 3).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "grep");
        assert_eq!(calls[1].id, "content_3_1");
        assert!(content_tool_calls(r#"{"tool_calls":[]}"#, &ToolBox::specs(), 1).is_none());
    }

    #[tokio::test]
    async fn content_finish_dispatches_and_preserves_verification_command() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text(
            r#"{"cmd":"python3 -m unittest","summary":"Done"}"#,
            FinishReason::Stop,
        )]);
        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into()).await.unwrap();
        assert_eq!(crate::proof::extract_cmd(&result).as_deref(), Some("python3 -m unittest"));
        assert_eq!(provider.requests().len(), 1);
    }

    #[test]
    fn verification_contract_requires_cmd_only_for_forge_tools() {
        let dir = tempfile::tempdir().unwrap();
        let mk = || agent(MockProvider::new(vec![]), dir.path());
        let finish = |agent: ForgeAgent| agent.request(Vec::new(), true, None).tools.into_iter()
            .find(|spec| spec.name == "finish").unwrap();
        assert_eq!(finish(mk()).parameters["required"], serde_json::json!(["summary"]));
        let spec = finish(mk().with_required_verification());
        assert_eq!(spec.parameters["required"], serde_json::json!(["summary", "cmd"]));
        assert_eq!(spec.parameters["properties"]["cmd"]["minLength"], 1);
        assert!(content_tool_calls(r#"{"summary":"Done"}"#, &[spec], 1).is_none());
    }

    #[tokio::test]
    async fn missing_finish_cmd_gets_one_repair_even_at_the_turn_limit() {
        for cmd in [None, Some(serde_json::json!("  ")), Some(serde_json::json!(false))] {
            let dir = tempfile::tempdir().unwrap();
            let mut args = serde_json::json!({"summary": "Done"});
            if let Some(cmd) = cmd { args["cmd"] = cmd; }
            let provider = MockProvider::new(vec![
                resp_tools(vec![tc("bad", "finish", args)]),
                resp_tools(vec![tc("good", "finish", serde_json::json!({"summary": "Done", "cmd": "python3 -m unittest"}))]),
            ]);
            let result = agent(provider.clone(), dir.path()).with_required_verification().with_max_turns(1)
                .run("system".into(), "task".into()).await.unwrap();
            assert_eq!(crate::proof::extract_cmd(&result).as_deref(), Some("python3 -m unittest"));
            let requests = provider.requests();
            assert_eq!(requests.len(), 2);
            let feedback = requests[1].messages.last().unwrap();
            assert_eq!(feedback.role, "tool");
            assert_eq!(feedback.tool_call_id.as_deref(), Some("bad"));
            assert!(feedback.content.starts_with("ERROR:"));
            assert!(feedback.content.contains("non-empty cmd string"));
        }
    }

    #[tokio::test]
    async fn turn_limit_keeps_the_verification_command_in_the_final_text_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("result.txt"), "done").unwrap();
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("read", "read_file", serde_json::json!({"path": "result.txt"}))]),
            resp_text("Done\ncmd: python3 -m unittest", FinishReason::Stop),
        ]);
        let result = agent(provider.clone(), dir.path()).with_required_verification().with_max_turns(1)
            .run("system".into(), "task".into()).await.unwrap();
        assert_eq!(crate::proof::extract_cmd(&result).as_deref(), Some("python3 -m unittest"));
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].tools.is_empty());
        assert!(requests[1].messages.last().unwrap().content.contains("CMD: <verification command>"));
    }

    #[tokio::test]
    async fn missing_cmd_repair_stops_after_one_nudge() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("first", "finish", serde_json::json!({"summary": "Done"}))]),
            resp_tools(vec![tc("second", "finish", serde_json::json!({"summary": "Still no command"}))]),
        ]);
        let result = agent(provider.clone(), dir.path()).with_required_verification()
            .run("system".into(), "task".into()).await.unwrap();
        assert_eq!(result, "Still no command");
        assert_eq!(provider.requests().len(), 2);
        assert!(crate::proof::extract_cmd(&result).is_none());
    }

    #[tokio::test]
    async fn text_completion_gets_a_nudge_without_guessing_a_command() {
        for text in [
            r#"{"summary":"Done"}"#,
            r#"{"name":"finish","arguments":{"summary":"Done","cmd":false}}"#,
            "Done. Run python3 -m unittest to check it.",
            "```json\n{\"cmd\":\"false\",\"summary\":\"Example\"}\n```",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let provider = MockProvider::new(vec![
                resp_text(text, FinishReason::Stop),
                resp_text(r#"{"summary":"Done","cmd":"python3 -m unittest"}"#, FinishReason::Stop),
            ]);
            let result = agent(provider.clone(), dir.path()).with_required_verification()
                .run("system".into(), "task".into()).await.unwrap();
            assert_eq!(crate::proof::extract_cmd(&result).as_deref(), Some("python3 -m unittest"));
            let requests = provider.requests();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].messages[2].content, text);
            assert!(requests[1].messages[2].tool_calls.is_none());
            assert_eq!(requests[1].messages[3].content, FINISH_CMD_NUDGE);
        }
    }

    #[tokio::test]
    async fn verification_repair_preserves_legacy_cmd_and_document_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("Done\nCMD: python3 -m unittest", FinishReason::Stop)]);
        let result = agent(provider.clone(), dir.path()).with_required_verification()
            .run("system".into(), "task".into()).await.unwrap();
        assert_eq!(crate::proof::extract_cmd(&result).as_deref(), Some("python3 -m unittest"));
        assert_eq!(provider.requests().len(), 1);
        for document in [true, false] {
            let provider = MockProvider::new(vec![resp_text("The document", FinishReason::Stop)]);
            let agent = agent(provider.clone(), dir.path()).with_required_verification();
            let agent = if document { agent.without_finish() } else { agent.with_no_tools() };
            assert_eq!(agent.run("system".into(), "task".into()).await.unwrap(), "The document");
            assert_eq!(provider.requests().len(), 1);
        }
    }

    #[tokio::test]
    async fn content_write_dispatches_and_replays_a_valid_tool_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![
            resp_text(r#"{"name":"write_file","arguments":{"path":"hello.py","content":"print('hello')\n"}}"#, FinishReason::Stop),
            resp_text("Done", FinishReason::Stop),
        ]);
        agent(provider.clone(), dir.path()).run("system".into(), "task".into()).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("hello.py")).unwrap(), "print('hello')\n");
        let requests = provider.requests();
        let messages = &requests[1].messages;
        let call = messages[2].tool_calls.as_ref().unwrap().first().unwrap();
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some(call.id.as_str()));
    }

    #[tokio::test]
    async fn tool_validation_error_reaches_model_without_invalid_tool_transcript() {
        struct ValidationProvider {
            requests: Mutex<Vec<ChatRequest>>,
            failures: usize,
        }
        impl Provider for ValidationProvider {
            fn chat(&self, req: ChatRequest) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>> {
                let mut requests = self.requests.lock().unwrap();
                requests.push(req);
                let invalid = requests.len() <= self.failures;
                Box::pin(async move {
                    if invalid {
                        Err(SlagError::ToolValidation("grep requires pattern".into()))
                    } else {
                        Ok(resp_tools(vec![tc("finish", "finish", serde_json::json!({"summary":"Done", "cmd":"true"}))]))
                    }
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(ValidationProvider { requests: Mutex::new(Vec::new()), failures: 1 });
        let result = ForgeAgent::new(provider.clone(), ToolBox::new(dir.path()), "test/model")
            .with_cost_cap(None).with_run_cap(None)
            .run("system".into(), "task".into()).await.unwrap();
        assert!(result.contains("CMD: true"));
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let correction = requests[1].messages.last().unwrap();
        assert_eq!(correction.role, "user");
        assert!(correction.content.contains("Tool error from provider: grep requires pattern"));
        assert!(requests[1].messages.iter().all(|m| m.tool_calls.is_none() && m.tool_call_id.is_none()));
        drop(requests);

        let repeating = Arc::new(ValidationProvider { requests: Mutex::new(Vec::new()), failures: usize::MAX });
        let error = ForgeAgent::new(repeating.clone(), ToolBox::new(dir.path()), "test/model")
            .with_cost_cap(None).with_run_cap(None)
            .run("system".into(), "task".into()).await.unwrap_err();
        assert!(matches!(error, SlagError::ToolValidation(_)));
        assert_eq!(repeating.requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn ingot_cost_cap_spans_sessions_via_the_shared_accumulator() {
        let dir = tempfile::tempdir().unwrap();
        let acc = SpendAccum::default();

        // Session 1 (heat 1): spends $0.04 of a $0.05 ingot cap, finishes.
        let mut first = resp_text("done", FinishReason::Stop);
        first.usage.cost = Some(0.04);
        agent(MockProvider::new(vec![first]), dir.path())
            .with_cost_cap(Some(0.05))
            .with_ingot_spend(acc.clone())
            .run("system".into(), "task".into())
            .await
            .expect("first session under cap");
        assert!((*acc.lock().unwrap() - 0.04).abs() < 1e-9);

        // Session 2 (heat 2, same ingot): $0.02 more crosses the cap; the
        // gate must see the cumulative $0.06, not a fresh $0.02.
        let read = tc("c1", "read_file", serde_json::json!({"path": "a.txt"}));
        let err = agent(
            MockProvider::new(vec![resp_tools_cost(vec![read], 0.02)]),
            dir.path(),
        )
        .with_cost_cap(Some(0.05))
        .with_ingot_spend(acc.clone())
        .run("system".into(), "task".into())
        .await
        .expect_err("second session must trip the shared cap");
        assert!(
            err.to_string().contains("ingot budget exhausted"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn run_cap_stops_a_session_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        // The global run-spend accumulator is always >= 0, so a zero cap
        // trips the gate deterministically at the first tool batch.
        let read = tc("c1", "read_file", serde_json::json!({"path": "a.txt"}));
        let err = agent(MockProvider::new(vec![resp_tools(vec![read])]), dir.path())
            .with_run_cap(Some(0.0))
            .run("system".into(), "task".into())
            .await
            .expect_err("run cap must stop the session");
        assert!(
            matches!(err, SlagError::RunBudgetExhausted { .. }),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn read_edit_finish_drives_a_real_file_change() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("src.txt"), "hello world\n").unwrap();

        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "src.txt"}))]),
            resp_tools(vec![tc(
                "c2",
                "edit_file",
                serde_json::json!({"path": "src.txt", "old_string": "hello", "new_string": "goodbye"}),
            )]),
            resp_tools(vec![tc(
                "c3",
                "finish",
                serde_json::json!({"summary": "swapped greeting"}),
            )]),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "swap the greeting".into())
            .await
            .expect("run ok");

        assert_eq!(result, "swapped greeting");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src.txt")).unwrap(),
            "goodbye world\n"
        );
        // finish short-circuited: exactly 3 model calls, all with tools.
        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|r| !r.tools.is_empty()));
    }

    #[tokio::test]
    async fn every_tool_call_id_gets_a_result_even_on_error() {
        let dir = tempfile::tempdir().unwrap();

        let provider = MockProvider::new(vec![
            resp_tools(vec![
                tc("c1", "read_file", serde_json::json!({"path": "missing.txt"})),
                tc("c2", "bogus_tool", serde_json::json!({})),
            ]),
            resp_text("done", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "done");

        // The second request carries a tool result for BOTH ids.
        let requests = provider.requests();
        let msgs = &requests[1].messages;
        let tool_msgs: Vec<&ChatMessage> = msgs.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(tool_msgs.len(), 2);
        assert_eq!(tool_msgs[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(tool_msgs[1].tool_call_id.as_deref(), Some("c2"));
        assert!(tool_msgs[0].content.starts_with("ERROR: "));
        assert!(tool_msgs[1].content.starts_with("ERROR: "));
    }

    #[tokio::test]
    async fn max_turns_forces_a_final_no_tools_summary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let read = || tc("c1", "read_file", serde_json::json!({"path": "a.txt"}));
        let provider = MockProvider::new(vec![
            resp_tools(vec![read()]),
            resp_tools(vec![read()]),
            resp_text("summary after budget", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .with_max_turns(2)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "summary after budget");

        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        let last = &requests[2];
        assert!(last.tools.is_empty(), "final call must offer no tools");
        let last_user = last.messages.iter().rev().find(|m| m.role == "user").unwrap();
        assert!(last_user.content.contains("no more tool budget"));
    }

    #[tokio::test]
    async fn length_truncation_first_retries_silently_with_a_raised_cap() {
        let dir = tempfile::tempdir().unwrap();

        let provider = MockProvider::new(vec![
            resp_text("partial", FinishReason::Length),
            resp_text("complete", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "complete");

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        // Silent escalation: same conversation re-sent (partial discarded,
        // no nudge appended), only max_tokens raised.
        assert_eq!(requests[0].max_tokens, None);
        assert_eq!(requests[1].max_tokens, Some(RAISED_MAX_TOKENS));
        assert_eq!(requests[0].messages.len(), requests[1].messages.len());
        assert!(requests[1]
            .messages
            .iter()
            .all(|m| m.content != "partial" && m.content != CONTINUE_NUDGE));
    }

    #[tokio::test]
    async fn truncation_at_the_raised_cap_gets_a_continuation_nudge() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![
            resp_text("partial", FinishReason::Length),
            resp_text("still partial", FinishReason::Length),
            resp_text("complete", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "complete");

        // Call 1 truncates → silent escalation; call 2 truncates at the
        // raised cap → the partial is kept and the resume nudge follows.
        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        let msgs = &requests[2].messages;
        assert_eq!(msgs[msgs.len() - 1].content, CONTINUE_NUDGE);
        assert_eq!(msgs[msgs.len() - 2].content, "still partial");
        assert_eq!(msgs[msgs.len() - 2].role, "assistant");
    }

    #[tokio::test]
    async fn persistent_truncation_fails_after_escalation_and_three_nudges() {
        let dir = tempfile::tempdir().unwrap();
        // 1 original + 1 escalated retry + 3 nudged retries, all truncated.
        let provider = MockProvider::new(vec![
            resp_text("p1", FinishReason::Length),
            resp_text("p2", FinishReason::Length),
            resp_text("p3", FinishReason::Length),
            resp_text("p4", FinishReason::Length),
            resp_text("p5", FinishReason::Length),
        ]);

        let err = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect_err("must fail, not loop or return a stump");
        assert!(err.to_string().contains("output truncated"), "got: {err}");
        assert_eq!(provider.requests().len(), 5);
    }

    /// Item 55: the silent escalation retry re-sends the committed history,
    /// never the failed attempt's partial. The turn watermark is what makes
    /// that true by construction rather than by luck of statement order.
    #[tokio::test]
    async fn an_escalated_retry_re_sends_the_committed_history_not_the_partial() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![
            resp_text("a partial that must not leak", FinishReason::Length),
            resp_text("complete", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "complete");

        let requests = provider.requests();
        assert_eq!(requests.len(), 2, "one escalated retry");
        assert_eq!(
            requests[1].messages.len(),
            requests[0].messages.len(),
            "the retry must carry the pre-attempt history, unchanged in length"
        );
        assert!(
            !requests[1]
                .messages
                .iter()
                .any(|m| m.content.contains("a partial that must not leak")),
            "the discarded partial leaked into the retried request"
        );
    }

    /// The nudge path is the deliberate exception: once escalation has been
    /// spent, the partial is kept on purpose so the model resumes from it.
    /// The watermark must not undo that.
    #[tokio::test]
    async fn the_nudge_path_still_keeps_its_partial_on_purpose() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![
            resp_text("first partial", FinishReason::Length),
            resp_text("second partial", FinishReason::Length),
            resp_text("complete", FinishReason::Stop),
        ]);

        agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let requests = provider.requests();
        let msgs = &requests[2].messages;
        assert_eq!(msgs[msgs.len() - 1].content, CONTINUE_NUDGE);
        assert_eq!(msgs[msgs.len() - 2].content, "second partial");
        // The escalated attempt's partial was still discarded; only the
        // post-escalation one is carried.
        assert!(
            !msgs.iter().any(|m| m.content == "first partial"),
            "escalation's partial must not survive alongside the nudged one"
        );
    }

    #[test]
    fn overflow_token_parser_reads_a_plus_b_gt_c() {
        assert_eq!(
            parse_overflow_tokens(
                "400: input length and max_tokens exceed context limit: 195017 + 21333 > 204698"
            ),
            Some((195017, 21333, 204698))
        );
        // Whitespace variants still parse.
        assert_eq!(parse_overflow_tokens("5000+9000>10000"), Some((5000, 9000, 10000)));
        // Plain phrasings without the arithmetic carry no numbers to act on.
        assert_eq!(parse_overflow_tokens("maximum context length is 8192 tokens"), None);
        assert_eq!(parse_overflow_tokens("no numbers at all"), None);
    }

    #[tokio::test]
    async fn output_only_overflow_shrinks_max_tokens_before_compacting() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FlakyProvider::new(vec![
            Err("400: input length and max_tokens exceed context limit: 5000 + 9000 > 10000"
                .into()),
            Ok(resp_text("done", FinishReason::Stop)),
        ]);

        let result = ForgeAgent::new(provider.clone(), ToolBox::new(dir.path()), "test/model")
            .run("system".into(), "task".into())
            .await
            .expect("shrunk-output retry must recover");
        assert_eq!(result, "done");

        let requests = provider.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        // max_tokens = limit - input - buffer; history untouched.
        assert_eq!(requests[1].max_tokens, Some((10000 - 5000 - 1024) as u32));
        assert_eq!(requests[0].messages.len(), requests[1].messages.len());
        assert!(requests[1]
            .messages
            .iter()
            .all(|m| !m.content.starts_with("[pruned old tool result")));
    }

    #[tokio::test]
    async fn stage_two_summarization_rescues_an_unprunable_overflow() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..8 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("content of f{i}\n"))
                .unwrap();
        }

        // 8 small read rounds (nothing stub-eligible at the char floor,
        // estimates far under budget — pruning cannot help), then a
        // context overflow: stage two must summarize and retry.
        let read = |i: usize| tc("c1", "read_file", serde_json::json!({"path": format!("f{i}.txt")}));
        let mut script: Vec<Result<NormalizedResponse, String>> =
            (0..8).map(|i| Ok(resp_tools(vec![read(i)]))).collect();
        script.push(Err("400: maximum context length exceeded".into()));
        script.push(Ok(resp_text("1..9 summary sections", FinishReason::Stop)));
        script.push(Ok(resp_text("done", FinishReason::Stop)));
        let provider = FlakyProvider::new(script);

        let result = ForgeAgent::new(provider.clone(), ToolBox::new(dir.path()), "test/model")
            .with_char_budget(400_000)
            .run("system".into(), "task".into())
            .await
            .expect("summarization must rescue the overflow");
        assert_eq!(result, "done");

        let requests = provider.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 11, "8 turns + overflow + summary call + retry");

        // The summary call itself: no tools, consequence-first preamble.
        let summary_req = &requests[9];
        assert!(summary_req.tools.is_empty());
        assert!(summary_req.messages[0]
            .content
            .starts_with("Respond with TEXT/JSON ONLY"));

        // The retried request: head replaced by the resume-silently
        // continuation; recently-read files whose reads fell out of the
        // tail are re-injected; tail-surviving reads are not.
        let retry = &requests[10];
        let continuation = retry
            .messages
            .iter()
            .find(|m| m.content.contains("continued from a previous conversation"))
            .expect("continuation message present");
        assert!(continuation.content.contains("1..9 summary sections"));
        assert!(continuation.content.contains("<system-reminder>"), "re-injection rides along");
        assert!(continuation.content.contains("## f3.txt"), "{}", continuation.content);
        assert!(continuation.content.contains("content of f4"), "{}", continuation.content);
        assert!(
            !continuation.content.contains("## f7.txt"),
            "tail-surviving read must not be re-injected"
        );
        // Tail rounds stay paired for strict backends.
        for (i, m) in retry.messages.iter().enumerate() {
            if m.role == "tool" {
                let id = m.tool_call_id.as_deref().unwrap();
                assert!(
                    retry.messages[..i].iter().any(|a| a
                        .tool_calls
                        .as_ref()
                        .is_some_and(|cs| cs.iter().any(|c| c.id == id))),
                    "orphan tool result after summarization"
                );
            }
        }
    }

    /// Regression: `summarize` hit the raw provider and dropped
    /// `resp.usage`, so up to 4 near-full-context LLM calls accrued zero
    /// spend — the ingot/run caps lied. The summarizer's cost must land
    /// in the shared accumulator.
    #[tokio::test]
    async fn summarizer_spend_lands_in_the_ingot_accumulator() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..8 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("content of f{i}\n"))
                .unwrap();
        }

        let read = |i: usize| tc("c1", "read_file", serde_json::json!({"path": format!("f{i}.txt")}));
        let mut script: Vec<Result<NormalizedResponse, String>> =
            (0..8).map(|i| Ok(resp_tools(vec![read(i)]))).collect();
        script.push(Err("400: maximum context length exceeded".into()));
        let mut summary = resp_text("1..9 summary sections", FinishReason::Stop);
        summary.usage.cost = Some(0.07);
        script.push(Ok(summary));
        script.push(Ok(resp_text("done", FinishReason::Stop)));
        let provider = FlakyProvider::new(script);

        let acc = SpendAccum::default();
        ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_char_budget(400_000)
            .with_cost_cap(None)
            .with_run_cap(None)
            .with_ingot_spend(acc.clone())
            .run("system".into(), "task".into())
            .await
            .expect("summarization must rescue the overflow");
        assert!(
            (*acc.lock().unwrap() - 0.07).abs() < 1e-9,
            "summarizer cost missing from the ingot accumulator: {}",
            *acc.lock().unwrap()
        );
    }

    /// Regression: post-compaction re-injection was a fixed 5 x 20k chars
    /// (~25k tokens) regardless of the shrunk budget — on a small window
    /// the summarized retry could exceed the very limit that triggered
    /// summarization. The cap now scales with the live token budget.
    #[test]
    fn reinject_cap_scales_with_the_token_budget() {
        // Large windows keep the fixed per-file ceiling.
        assert_eq!(reinject_file_cap(150_000), REINJECT_FILE_CAP);
        // At the floor the whole re-injection fits in a quarter of the
        // budget and never disappears entirely.
        let cap = reinject_file_cap(TOKEN_BUDGET_FLOOR);
        assert!(cap > 0);
        assert!(
            cap * REINJECT_FILES <= TOKEN_BUDGET_FLOOR * CHARS_PER_TOKEN / REINJECT_BUDGET_DIV,
            "re-injection ({} chars) must fit the floored budget",
            cap * REINJECT_FILES
        );
    }

    #[tokio::test]
    async fn writer_closes_segment_so_batch_runs_in_model_order() {
        let dir = tempfile::tempdir().unwrap();

        // write then read of the same file in one batch: the writer must
        // complete before the read runs, so the read sees the new content.
        let provider = MockProvider::new(vec![
            resp_tools(vec![
                tc(
                    "c1",
                    "write_file",
                    serde_json::json!({"path": "f.txt", "content": "forged\n"}),
                ),
                tc("c2", "read_file", serde_json::json!({"path": "f.txt"})),
            ]),
            resp_text("done", FinishReason::Stop),
        ]);

        agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let requests = provider.requests();
        let read_result = requests[1]
            .messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c2"))
            .expect("read result present");
        assert!(
            read_result.content.contains("forged"),
            "read saw: {}",
            read_result.content
        );
    }

    #[test]
    fn plan_segments_groups_readers_and_isolates_writers() {
        let calls = vec![
            tc("1", "read_file", serde_json::json!({"path": "a"})),
            tc("2", "read_file", serde_json::json!({"path": "b"})),
            tc("3", "write_file", serde_json::json!({"path": "c", "content": ""})),
            // Unproven bash stays isolated; read-only bash (item 96)
            // joins the reader batch after it.
            tc("4", "bash", serde_json::json!({"command": "cargo build"})),
            tc("5", "bash", serde_json::json!({"command": "ls"})),
            tc("6", "read_file", serde_json::json!({"path": "d"})),
        ];
        let segments = plan_segments(&calls);
        assert_eq!(segments, vec![vec![0, 1], vec![2], vec![3], vec![4, 5]]);
    }

    #[tokio::test]
    async fn finish_preserves_same_turn_assistant_text() {
        let dir = tempfile::tempdir().unwrap();

        // Plan mode contract: the plan arrives as final text on the finish
        // turn; only the one-line summary rides the finish call.
        let mut resp = resp_tools(vec![tc(
            "c1",
            "finish",
            serde_json::json!({"summary": "plan drafted"}),
        )]);
        resp.content = "## Plan\n1. do the thing".into();
        let provider = MockProvider::new(vec![resp]);

        let result = agent(provider, dir.path())
            .run("system".into(), "plan it".into())
            .await
            .expect("run ok");
        assert!(result.contains("## Plan\n1. do the thing"), "got: {result}");
        assert!(result.contains("plan drafted"), "got: {result}");
    }

    #[tokio::test]
    async fn reasoning_details_replay_with_assistant_tool_calls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let details = serde_json::json!([{"type": "reasoning.encrypted", "data": "opaque"}]);
        let mut first = resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]);
        first.reasoning_details = Some(details.clone());
        let provider = MockProvider::new(vec![first, resp_text("done", FinishReason::Stop)]);

        agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        // The second request must replay the reasoning blocks on the
        // assistant message that carried the tool calls.
        let requests = provider.requests();
        let assistant = requests[1]
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("assistant message present");
        assert_eq!(assistant.reasoning_details.as_ref(), Some(&details));
    }

    #[tokio::test]
    async fn finish_alongside_other_calls_still_answers_all_ids() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let provider = MockProvider::new(vec![resp_tools(vec![
            tc("c1", "read_file", serde_json::json!({"path": "a.txt"})),
            tc("c2", "finish", serde_json::json!({"summary": "all done"})),
        ])]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "all done");
        // Short-circuit: only one model call ever happened.
        assert_eq!(provider.requests().len(), 1);
    }

    /// Wraps MockProvider with a per-call hook — the only way to inject a
    /// steer or flip the cancel flag *between* turns of a running loop.
    struct HookedProvider {
        inner: Arc<MockProvider>,
        hook: Box<dyn Fn(usize) + Send + Sync>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Provider for HookedProvider {
        fn chat(
            &self,
            req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            (self.hook)(n);
            self.inner.chat(req)
        }
    }

    #[tokio::test]
    async fn steer_before_first_turn_becomes_a_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("done", FinishReason::Stop)]);
        let queue: SteerQueue = Arc::new(Mutex::new(vec!["focus on tests".into()]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let result = agent(provider.clone(), dir.path())
            .with_steer(queue.clone())
            .with_events(tx)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "done");
        assert!(queue.lock().unwrap().is_empty(), "queue drained");

        let msgs = &provider.requests()[0].messages;
        let steer_msg = msgs
            .iter()
            .find(|m| m.content.contains("focus on tests"))
            .expect("steer message present");
        assert_eq!(steer_msg.role, "user");
        assert!(
            steer_msg.content.starts_with(crate::engine::prompt::REMINDER_OPEN),
            "steers ride the standard reminder envelope: {}",
            steer_msg.content
        );
        assert!(steer_msg.content.contains(STEER_TAG));

        let mut saw_steer_event = false;
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::Steer { text } = ev {
                assert_eq!(text, "focus on tests");
                saw_steer_event = true;
            }
        }
        assert!(saw_steer_event, "Steer event emitted");
    }

    #[tokio::test]
    async fn an_external_file_change_is_injected_before_the_next_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();

        // Turn 1 reads the file. Turn 2 rewrites it through bash, which
        // does not arm the read-before-edit gate — exactly the shape of a
        // parallel anvil's write. Turn 3's request must carry the reminder.
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]),
            resp_tools(vec![tc(
                "c2",
                "bash",
                serde_json::json!({"command": "sleep 0.02 && printf 'one\\nTWO\\n' > a.txt"}),
            )]),
            resp_text("done", FinishReason::Stop),
        ]);

        agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let second = &provider.requests()[2].messages;
        let note = second
            .iter()
            .find(|m| m.content.contains("modified externally"))
            .expect("external-change reminder present");
        assert!(
            note.content.contains(crate::engine::prompt::REMINDER_OPEN),
            "rides the reminder envelope: {}",
            note.content
        );
        assert!(note.content.contains("-two"), "{}", note.content);
        assert!(note.content.contains("+TWO"), "{}", note.content);
    }

    #[tokio::test]
    async fn multiple_first_turn_steers_keep_queue_order() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("done", FinishReason::Stop)]);
        let queue: SteerQueue = Arc::new(Mutex::new(vec!["first".into(), "second".into()]));

        agent(provider.clone(), dir.path())
            .with_steer(queue)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let msgs = &provider.requests()[0].messages;
        let carrier = msgs
            .iter()
            .find(|m| m.content.contains("first"))
            .expect("steer carrier present");
        assert_eq!(
            carrier.content.matches(crate::engine::prompt::REMINDER_OPEN).count(),
            1,
            "both steers coalesce into one envelope: {}",
            carrier.content
        );
        let first = carrier.content.find("\nfirst").unwrap();
        let second = carrier.content.find("\nsecond").unwrap();
        assert!(first < second, "steers applied in queue order");
    }

    #[tokio::test]
    async fn steers_during_tools_concatenate_into_last_tool_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let inner = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]),
            resp_text("done", FinishReason::Stop),
        ]);
        // Steers arrive during the first model call: after the turn-1 drain,
        // before the post-tool drain — they must land in the tool result.
        let queue: SteerQueue = Arc::new(Mutex::new(Vec::new()));
        let q = queue.clone();
        let provider = Arc::new(HookedProvider {
            inner: inner.clone(),
            hook: Box::new(move |n| {
                if n == 0 {
                    let mut steers = q.lock().unwrap();
                    steers.push("first".into());
                    steers.push("second".into());
                }
            }),
            calls: Default::default(),
        });

        ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_steer(queue)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let requests = inner.requests();
        let tool_msg = requests[1]
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "tool")
            .expect("tool result present");
        let i1 = tool_msg
            .content
            .find(&format!("{STEER_TAG}\nfirst"))
            .expect("first steer in tool result");
        let i2 = tool_msg
            .content
            .find(&format!("{STEER_TAG}\nsecond"))
            .expect("second steer in tool result");
        assert!(i1 > 0, "steer appended after the tool output");
        assert!(i1 < i2, "steers concatenate in order");
        // No steer leaked into a user message.
        assert!(requests[1]
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .all(|m| !m.content.contains(STEER_TAG)));
    }

    /// Scripted provider that can also fail: pops one Result per chat call.
    struct FlakyProvider {
        script: Mutex<VecDeque<Result<NormalizedResponse, String>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl FlakyProvider {
        fn new(script: Vec<Result<NormalizedResponse, String>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    impl Provider for FlakyProvider {
        fn chat(
            &self,
            req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            self.requests.lock().unwrap().push(req);
            let next = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("flaky script exhausted");
            Box::pin(async move { next.map_err(SlagError::Provider) })
        }
    }

    #[test]
    fn context_overflow_classifier_matches_provider_phrasings() {
        for msg in [
            "400 Bad Request: This endpoint's maximum context length is 131072 tokens",
            "400: context_length_exceeded",
            "400: input is too long for this model's context window",
        ] {
            assert!(is_context_overflow(&SlagError::Provider(msg.into())), "{msg}");
        }
        assert!(!is_context_overflow(&SlagError::Provider("429: rate limited".into())));
        assert!(!is_context_overflow(&SlagError::Provider("500: internal".into())));
        assert!(!is_context_overflow(&SlagError::ToolValidation(
            "Tool call validation failed: context length argument is missing".into(),
        )));
    }

    #[tokio::test]
    async fn context_overflow_compacts_harder_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), format!("{}\n", "y".repeat(6000))).unwrap();

        let read = || tc("c1", "read_file", serde_json::json!({"path": "big.txt"}));
        // Six fat read turns build prunable history beyond the protected
        // tail, then the provider rejects on context, then accepts the
        // harder-compacted retry.
        let provider = FlakyProvider::new(vec![
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Err("400 Bad Request: maximum context length is 8192 tokens".into()),
            Ok(resp_text("done", FinishReason::Stop)),
        ]);

        let result = ForgeAgent::new(provider.clone(), ToolBox::new(dir.path()), "test/model")
            .with_char_budget(40_000)
            .run("system".into(), "task".into())
            .await
            .expect("overflow must be recovered by compaction, not propagated");
        assert_eq!(result, "done");

        let requests = provider.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 8, "exactly one retry after the overflow");
        // The retry request carries a pruned (stubbed) old tool result.
        let retry = &requests[7];
        assert!(
            retry.messages.iter().any(|m| m.content.starts_with("[pruned old tool result")),
            "retry must be compacted"
        );
    }

    #[tokio::test]
    async fn first_turn_overflow_with_nothing_prunable_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FlakyProvider::new(vec![
            Err("400: maximum context length exceeded".into()),
        ]);
        let err = ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_char_budget(40_000)
            .run("system".into(), "task".into())
            .await
            .expect_err("nothing prunable — must fail, not loop");
        assert!(err.to_string().contains("context"), "got: {err}");
    }

    #[tokio::test]
    async fn steers_are_requeued_when_the_provider_errors() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FlakyProvider::new(vec![Err("500: boom".into())]);
        let queue: SteerQueue = Arc::new(Mutex::new(vec!["do NOT touch migrations".into()]));

        let err = ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_steer(queue.clone())
            .run("system".into(), "task".into())
            .await
            .expect_err("provider error propagates");
        assert!(err.to_string().contains("boom"), "got: {err}");

        // The drained steer is back in the queue for the retry heat.
        assert_eq!(*queue.lock().unwrap(), vec!["do NOT touch migrations".to_string()]);
    }

    fn resp_tools_cost(calls: Vec<ToolCall>, cost: f64) -> NormalizedResponse {
        let mut resp = resp_tools(calls);
        resp.usage.cost = Some(cost);
        resp
    }

    #[tokio::test]
    async fn cost_cap_warns_at_80_percent_and_fails_at_100() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let read = || tc("c1", "read_file", serde_json::json!({"path": "a.txt"}));
        // $0.40 per turn against a $1.00 cap: turn 2 crosses 80%, turn 3
        // crosses 100% and must fail before dispatching its tools.
        let provider = MockProvider::new(vec![
            resp_tools_cost(vec![read()], 0.40),
            resp_tools_cost(vec![read()], 0.40),
            resp_tools_cost(vec![read()], 0.40),
        ]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let err = agent(provider.clone(), dir.path())
            .with_cost_cap(Some(1.0))
            .with_events(tx)
            .run("system".into(), "task".into())
            .await
            .expect_err("cap must stop the session");
        assert!(err.to_string().contains("ingot budget exhausted"), "got: {err}");
        assert!(err.to_string().contains("$1.2000"), "got: {err}");
        assert_eq!(provider.requests().len(), 3, "no call after the cap trips");

        let mut warnings = 0;
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::Warning { message } = ev {
                assert!(message.contains("80%"), "message: {message}");
                warnings += 1;
            }
        }
        assert_eq!(warnings, 1, "exactly one 80% warning");
    }

    #[tokio::test]
    async fn under_cap_session_completes_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let mut resp = resp_text("done", FinishReason::Stop);
        resp.usage.cost = Some(0.10);
        let provider = MockProvider::new(vec![resp]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let result = agent(provider, dir.path())
            .with_cost_cap(Some(10.0))
            .with_events(tx)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "done");

        while let Ok(ev) = rx.try_recv() {
            assert!(
                !matches!(ev, EngineEvent::Warning { .. }),
                "no warning under 80%"
            );
        }
    }

    #[tokio::test]
    async fn shrink_breaker_fails_after_three_compaction_cycles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), format!("{}\n", "y".repeat(6000))).unwrap();

        let read = || tc("c1", "read_file", serde_json::json!({"path": "big.txt"}));
        let overflow =
            || Err("400 Bad Request: maximum context length is 8192 tokens".to_string());
        // 30 fat read turns build ~180k chars of prunable history, then the
        // provider rejects on context four times in a row. Compaction keeps
        // making progress (so the no-progress guard never fires), but the
        // fourth overflow must trip the circuit breaker.
        let mut script: Vec<Result<NormalizedResponse, String>> =
            (0..30).map(|_| Ok(resp_tools(vec![read()]))).collect();
        script.extend([overflow(), overflow(), overflow(), overflow()]);
        let provider = FlakyProvider::new(script);

        let err = ForgeAgent::new(provider.clone(), ToolBox::new(dir.path()), "test/model")
            .with_char_budget(160_000)
            .run("system".into(), "task".into())
            .await
            .expect_err("breaker must fail, not loop");
        assert!(
            err.to_string().contains("context irrecoverable after 3 compactions"),
            "got: {err}"
        );
        // 30 good turns + the overflow and its 3 compacted retries.
        assert_eq!(provider.requests.lock().unwrap().len(), 34);
    }

    #[tokio::test]
    async fn cancel_mid_batch_skips_tools_and_backfills_results() {
        let dir = tempfile::tempdir().unwrap();

        let inner = MockProvider::new(vec![
            resp_tools(vec![tc(
                "c1",
                "write_file",
                serde_json::json!({"path": "f.txt", "content": "should never land\n"}),
            )]),
            resp_text("never reached", FinishReason::Stop),
        ]);
        // Flag flips during the first model call: the batch must not run
        // its writer, but its tool_call_id still gets a (backfilled) result
        // before the turn-boundary check aborts the loop.
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = flag.clone();
        let provider = Arc::new(HookedProvider {
            inner: inner.clone(),
            hook: Box::new(move |n| {
                if n == 0 {
                    f.store(true, Ordering::SeqCst);
                }
            }),
            calls: Default::default(),
        });

        let err = ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_cancel(flag)
            .run("system".into(), "task".into())
            .await
            .expect_err("cancel aborts the run");
        assert!(err.to_string().contains("interrupted by user"), "got: {err}");
        assert_eq!(inner.requests().len(), 1);
        assert!(
            !dir.path().join("f.txt").exists(),
            "writer must not run after cancel"
        );
    }

    /// Item 56, the clause the reason enum exists for: an operator who
    /// interrupts to steer is redirecting live work, not hitting a failure.
    /// The run still stops, but it must not report an error.
    #[tokio::test]
    async fn a_steer_interrupt_narrates_instead_of_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = Cancellation::default();
        cancel.raise(CancelReason::SteerInterrupt);

        let err = agent(MockProvider::new(vec![]), dir.path())
            .with_events(tx)
            .with_cancellation(cancel)
            .run("system".into(), "task".into())
            .await
            .expect_err("a raised interrupt still ends the run");
        assert!(matches!(err, SlagError::Cancelled), "got: {err}");

        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert!(
            !events.iter().any(|e| matches!(e, EngineEvent::Error { .. })),
            "a steer must skip the error path: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                EngineEvent::Narrate { text } if text.contains(CancelReason::SteerInterrupt.label())
            )),
            "the reason must reach the operator: {events:?}"
        );
    }

    /// The default reason is unchanged: a ctrl-C is still a failure the
    /// assay reports, so it keeps the error event.
    #[tokio::test]
    async fn an_operator_abort_still_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = Cancellation::default();
        cancel.raise(CancelReason::UserAbort);

        let _ = agent(MockProvider::new(vec![]), dir.path())
            .with_events(tx)
            .with_cancellation(cancel)
            .run("system".into(), "task".into())
            .await;

        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert!(
            events.iter().any(|e| matches!(e, EngineEvent::Error { .. })),
            "an abort still errors: {events:?}"
        );
    }

    /// `with_cancel` remains the bridge for the surfaces that only hold a
    /// bare flag: adopting one reads as an abort, the conservative default.
    #[tokio::test]
    async fn a_bare_flag_still_reads_as_an_abort() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let _ = agent(MockProvider::new(vec![]), dir.path())
            .with_events(tx)
            .with_cancel(flag)
            .run("system".into(), "task".into())
            .await;

        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert!(
            events.iter().any(|e| matches!(e, EngineEvent::Error { .. })),
            "an adopted flag defaults to abort: {events:?}"
        );
    }

    #[tokio::test]
    async fn dispatch_batch_backfills_every_id_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let provider = MockProvider::new(vec![]);
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let forge_agent = agent(provider, dir.path()).with_cancel(flag);

        let calls = vec![
            tc("c1", "read_file", serde_json::json!({"path": "a.txt"})),
            tc("c2", "bash", serde_json::json!({"command": "echo hi"})),
        ];
        let outcomes = forge_agent.dispatch_batch(&calls).await;

        assert_eq!(outcomes.len(), 2, "one outcome per tool_call_id");
        for outcome in &outcomes {
            assert!(outcome.is_error);
            assert!(outcome.output.contains("cancelled"), "got: {}", outcome.output);
        }
    }

    #[tokio::test]
    async fn preset_cancel_aborts_before_any_provider_call() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("never", FinishReason::Stop)]);
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let err = agent(provider.clone(), dir.path())
            .with_cancel(flag)
            .run("system".into(), "task".into())
            .await
            .expect_err("cancel must abort the run");
        assert!(err.to_string().contains("interrupted by user"), "got: {err}");
        assert_eq!(provider.requests().len(), 0, "no provider call after cancel");
    }

    #[tokio::test]
    async fn mid_run_cancel_aborts_before_the_next_provider_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let inner = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]),
            resp_text("never reached", FinishReason::Stop),
        ]);
        // Flag flips during the first model call: tools still run, but the
        // turn-2 boundary check must stop the loop.
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = flag.clone();
        let provider = Arc::new(HookedProvider {
            inner: inner.clone(),
            hook: Box::new(move |n| {
                if n == 0 {
                    f.store(true, Ordering::SeqCst);
                }
            }),
            calls: Default::default(),
        });

        let err = ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_cancel(flag)
            .run("system".into(), "task".into())
            .await
            .expect_err("cancel must abort the run");
        assert!(err.to_string().contains("interrupted by user"), "got: {err}");
        assert_eq!(inner.requests().len(), 1, "exactly one call before the flag check");
    }

    #[test]
    fn token_estimate_prefers_the_usage_anchor_over_chars() {
        let messages = vec![
            ChatMessage::system("s".repeat(4000)),
            ChatMessage::user("t".repeat(4000)),
            ChatMessage::user("delta".repeat(80)), // 400 chars appended since
        ];
        // No anchor: pure chars/4 over everything.
        assert_eq!(estimate_tokens(&messages, None), (8400 / CHARS_PER_TOKEN) as u64);
        // Anchored: exact count for the first two, chars/4 for the delta
        // only — never a cumulative sum of per-turn usage.
        let anchor = TokenAnchor { tokens: 50_000, sent: 2 };
        assert_eq!(
            estimate_tokens(&messages, Some(anchor)),
            50_000 + (400 / CHARS_PER_TOKEN) as u64
        );
        // A stale anchor pointing past the end falls back to chars/4.
        let stale = TokenAnchor { tokens: 50_000, sent: 99 };
        assert_eq!(estimate_tokens(&messages, Some(stale)), (8400 / CHARS_PER_TOKEN) as u64);
    }

    #[test]
    fn window_budget_subtracts_reserves_and_floors() {
        assert_eq!(
            budget_for_window(131_072),
            131_072 - (OUTPUT_RESERVE_TOKENS + COMPACT_BUFFER_TOKENS) as usize
        );
        // A window smaller than the reserves still leaves a working floor.
        assert_eq!(budget_for_window(8_192), TOKEN_BUDGET_FLOOR);
        assert_eq!(budget_for_window(0), TOKEN_BUDGET_FLOOR);
    }

    #[test]
    fn with_context_window_derives_the_token_budget() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![]);
        let base = agent(provider.clone(), dir.path());
        let default_budget = base.token_budget;

        let sized = agent(provider.clone(), dir.path()).with_context_window(Some(131_072));
        assert_eq!(sized.token_budget, DEFAULT_TOKEN_BUDGET);
        // Unknown window keeps whatever budget was already in place.
        let unknown = agent(provider, dir.path()).with_context_window(None);
        assert_eq!(unknown.token_budget, default_budget);
    }

    #[test]
    fn char_budget_env_parses_chars_into_tokens() {
        assert_eq!(parse_char_budget(None), None);
        assert_eq!(parse_char_budget(Some("400000".into())), Some(100_000));
        assert_eq!(parse_char_budget(Some("0".into())), None);
        assert_eq!(parse_char_budget(Some("junk".into())), None);
    }

    #[tokio::test]
    async fn usage_anchor_triggers_compaction_before_chars_would() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..4 {
            std::fs::write(dir.path().join(format!("a{i}.txt")), format!("A{i}").repeat(500))
                .unwrap();
        }

        // Four fat-token turns: the provider reports a prompt far beyond
        // the budget while the transcript stays tiny in chars (~4k, worth
        // ~1k tokens by chars/4). Only the usage anchor can see trouble.
        let read = |i: usize| {
            tc("c1", "read_file", serde_json::json!({"path": format!("a{i}.txt")}))
        };
        let mut script: Vec<NormalizedResponse> = (0..4)
            .map(|i| {
                let mut resp = resp_tools(vec![read(i)]);
                resp.usage.prompt_tokens = 999_999;
                resp
            })
            .collect();
        script.push(resp_text("done", FinishReason::Stop));
        let provider = MockProvider::new(script);

        let result = agent(provider.clone(), dir.path())
            .with_char_budget(400_000) // 100k tokens — chars/4 never trips it
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "done");

        // The 5th request must be compacted: the anchored estimate crossed
        // the budget even though raw chars sat far under it.
        let requests = provider.requests();
        let last = &requests[4].messages;
        let compacted = last.len() < 10
            || last.iter().any(|m| m.content.starts_with("[pruned old tool result"));
        assert!(compacted, "anchored estimate must drive compaction");
        // Whatever was pruned, pairing stays intact for strict backends.
        for (i, m) in last.iter().enumerate() {
            if m.role == "tool" {
                let id = m.tool_call_id.as_deref().unwrap();
                assert!(
                    last[..i].iter().any(|a| a
                        .tool_calls
                        .as_ref()
                        .is_some_and(|cs| cs.iter().any(|c| c.id == id))),
                    "orphan tool result in compacted request"
                );
            }
        }
    }

    #[test]
    fn mtok_rate_parses_overrides_and_falls_back() {
        assert_eq!(mtok_rate_from(None), COST_PER_MTOK_DEFAULT);
        assert_eq!(mtok_rate_from(Some("2.5")), 2.5);
        assert_eq!(mtok_rate_from(Some(" 10 ")), 10.0);
        assert_eq!(mtok_rate_from(Some("0")), 0.0);
        assert_eq!(mtok_rate_from(Some("-1")), COST_PER_MTOK_DEFAULT);
        assert_eq!(mtok_rate_from(Some("NaN")), COST_PER_MTOK_DEFAULT);
        assert_eq!(mtok_rate_from(Some("junk")), COST_PER_MTOK_DEFAULT);
    }

    #[test]
    fn spend_for_trusts_a_reported_zero_cost_over_the_estimate() {
        // OpenRouter `:free` variants report cost: 0.0 on every response.
        // 0.0 means free, not missing — the estimate must not override it.
        let free = Usage { total_tokens: 200_000, cost: Some(0.0), ..Default::default() };
        assert_eq!(spend_for(&free), 0.0);
        // Absent cost still falls back to the token estimate.
        let stripped = Usage { total_tokens: 200_000, cost: None, ..Default::default() };
        assert!(spend_for(&stripped) > 0.0);
        // A negative report is nonsense — treated as absent.
        let junk = Usage { total_tokens: 200_000, cost: Some(-1.0), ..Default::default() };
        assert!(spend_for(&junk) > 0.0);
    }

    #[tokio::test]
    async fn free_model_run_accrues_no_phantom_spend() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let acc = SpendAccum::default();

        // A long free-model turn: huge token counts, reported cost 0.0.
        // Estimated at $5/Mtok this would be ~$50 of phantom spend and
        // trip the $1 ingot cap; the reported zero must win.
        let read = tc("c1", "read_file", serde_json::json!({"path": "a.txt"}));
        let mut turn = resp_tools(vec![read]);
        turn.usage.total_tokens = 10_000_000;
        turn.usage.cost = Some(0.0);
        let mut done = resp_text("done", FinishReason::Stop);
        done.usage.total_tokens = 10_000_000;
        done.usage.cost = Some(0.0);

        let result = agent(MockProvider::new(vec![turn, done]), dir.path())
            .with_cost_cap(Some(1.0))
            .with_ingot_spend(acc.clone())
            .run("system".into(), "task".into())
            .await
            .expect("free model must not trip the cap");
        assert_eq!(result, "done");
        assert_eq!(*acc.lock().unwrap(), 0.0, "no phantom spend accrued");
    }

    #[tokio::test]
    async fn spend_tracked_provider_folds_judge_cost_into_the_accumulator() {
        let mut resp = resp_text("verdict", FinishReason::Stop);
        resp.usage.cost = Some(0.5);
        let inner = MockProvider {
            script: Mutex::new(vec![resp].into()),
            requests: Mutex::new(Vec::new()),
        };
        let acc = SpendAccum::default();
        let tracked = SpendTracked::new(inner, acc.clone()).with_run_cap(None);

        let req = ChatRequest {
            model: "judge/model".into(),
            messages: vec![ChatMessage::user("rule")],
            tools: vec![],
            effort: None,
            max_tokens: None,
            role: Role::Judge,
            retry: RetryPolicy::side(),
        };
        tracked.chat(req).await.expect("chat ok");
        assert!((*acc.lock().unwrap() - 0.5).abs() < 1e-9, "judge cost tracked");
    }

    #[tokio::test]
    async fn spend_tracked_provider_refuses_once_the_run_cap_is_spent() {
        let inner = MockProvider {
            script: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        };
        // The global run-spend accumulator is always >= 0, so a zero cap
        // trips deterministically before any provider call.
        let tracked = SpendTracked::new(inner, SpendAccum::default()).with_run_cap(Some(0.0));

        let req = ChatRequest {
            model: "judge/model".into(),
            messages: vec![ChatMessage::user("rule")],
            tools: vec![],
            effort: None,
            max_tokens: None,
            role: Role::Judge,
            retry: RetryPolicy::side(),
        };
        let err = tracked.chat(req).await.expect_err("run cap must refuse");
        assert!(matches!(err, SlagError::RunBudgetExhausted { .. }), "got: {err}");
        assert!(tracked.inner.requests.lock().unwrap().is_empty(), "no call past the cap");
    }

    #[test]
    fn missing_cost_is_estimated_from_tokens_so_caps_still_bind() {
        // 1M tokens at $5/Mtok — the estimate keeps budget caps binding
        // when a proxy strips usage.cost from every response.
        let usage = Usage { total_tokens: 1_000_000, cost: None, ..Default::default() };
        assert!((estimated_cost_at(&usage, 5.0) - 5.0).abs() < 1e-9);
        // Cheaper override rate scales linearly.
        assert!((estimated_cost_at(&usage, 0.5) - 0.5).abs() < 1e-9);
        // No tokens, no spend: the accumulator ignores zero estimates.
        let idle = Usage { total_tokens: 0, cost: None, ..Default::default() };
        assert_eq!(estimated_cost_at(&idle, 5.0), 0.0);
    }

    // ─── Transcripts (item 67) ───

    /// Inside a forge attempt scope the whole session journals to
    /// logs/transcripts/<ingot>-h<heat>.jsonl, closed by an `end` entry.
    #[tokio::test]
    async fn scoped_session_journals_messages_and_closes_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]),
            resp_text("done", FinishReason::Stop),
        ]);
        let a = agent(provider, dir.path()).with_transcript_root(dir.path());

        transcript::scope("i1".into(), 2, a.run("system".into(), "task".into()))
            .await
            .expect("run ok");

        let path = transcript::path_for(dir.path(), "i1", 2);
        let raw = std::fs::read_to_string(&path).expect("transcript written");
        // system, task, assistant tool_calls, tool result, and the end.
        assert_eq!(raw.lines().count(), 5, "{raw}");
        assert!(raw.contains("\"entry\":\"end\""), "{raw}");
        assert!(raw.contains("\"tool_call_id\":\"c1\""), "{raw}");
        // Closed transcripts never resume.
        assert!(!transcript::is_resumable(dir.path(), "i1", 2));
    }

    /// A crashed session's open transcript resumes: the next run at the
    /// same (ingot, heat) replays the recorded history instead of
    /// starting over from system + task.
    #[tokio::test]
    async fn open_transcript_resumes_the_recorded_conversation() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate the crash artifact: an open transcript with real
        // progress (a tool round) and no end entry.
        let w = transcript::TranscriptWriter::new(transcript::path_for(dir.path(), "i1", 3));
        let call = tc("c1", "bash", serde_json::json!({"command": "cargo test"}));
        w.begin(&[ChatMessage::system("original system"), ChatMessage::user("original task")]);
        w.record(&ChatMessage::assistant("running tests", Some(vec![call])));
        w.record(&ChatMessage::tool_result("c1", "212 passed"));

        let provider = MockProvider::new(vec![resp_text("finished the ingot", FinishReason::Stop)]);
        let a = agent(provider.clone(), dir.path())
            .with_transcript_root(dir.path())
            .with_resume(true);
        let result = transcript::scope(
            "i1".into(),
            3,
            a.run("fresh system (ignored)".into(), "fresh task (ignored)".into()),
        )
        .await
        .expect("resumed run ok");
        assert_eq!(result, "finished the ingot");

        // The one model call carried the recorded history, not the fresh
        // system/task pair.
        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        let msgs = &requests[0].messages;
        assert_eq!(msgs[0].content, "original system");
        assert_eq!(msgs[1].content, "original task");
        assert!(msgs.iter().any(|m| m.content == "212 passed"));
        assert!(msgs.iter().all(|m| !m.content.contains("fresh system")));

        // The resumed session closed the transcript: no double resume.
        assert!(!transcript::is_resumable(dir.path(), "i1", 3));
    }

    /// Without the explicit resume opt-in, a stale open transcript from a
    /// previous job at the same (ingot, heat) must NOT hijack a fresh
    /// heat: the new session begins from its own system+task and the
    /// stale file is truncated.
    #[tokio::test]
    async fn fresh_strike_ignores_a_stale_open_transcript() {
        let dir = tempfile::tempdir().unwrap();
        // Stale artifact from a killed prior job: open, with progress.
        let path = transcript::path_for(dir.path(), "i1", 1);
        let w = transcript::TranscriptWriter::new(path.clone());
        w.begin(&[ChatMessage::system("old job system"), ChatMessage::user("old job task")]);
        w.record(&ChatMessage::assistant("old job progress", None));
        assert!(transcript::is_resumable(dir.path(), "i1", 1));

        let provider = MockProvider::new(vec![resp_text("done", FinishReason::Stop)]);
        let a = agent(provider.clone(), dir.path()).with_transcript_root(dir.path());
        transcript::scope("i1".into(), 1, a.run("new system".into(), "new task".into()))
            .await
            .expect("fresh run ok");

        // The model saw the new job's conversation, not the stale one.
        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages[0].content, "new system");
        assert!(requests[0].messages.iter().all(|m| !m.content.contains("old job")));

        // And the stale content is gone from the journal.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("old job"), "{raw}");
    }

    /// Outside a scope (plan passes, duel casts, tests) nothing journals.
    #[tokio::test]
    async fn unscoped_sessions_write_no_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("done", FinishReason::Stop)]);
        agent(provider, dir.path())
            .with_transcript_root(dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert!(!dir.path().join(transcript::TRANSCRIPT_DIR).exists());
    }
}
