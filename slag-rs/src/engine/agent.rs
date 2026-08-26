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
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use tokio::task::JoinHandle;

use super::compact::{compact, convo_chars, summarize};
use super::events::preview;
use super::tools::ToolBox;
use super::transcript::{self, TranscriptWriter};
use super::{
    emit, stats, CancelFlag, ChatMessage, ChatRequest, Effort, EngineEvent, EventTx, FinishReason,
    NormalizedResponse, Provider, SteerQueue, ToolCall, ToolOutcome, Usage,
};
use crate::error::SlagError;

const DEFAULT_MAX_TURNS: usize = 40;
const CHAR_BUDGET: usize = 600_000;
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
    cancel: Option<CancelFlag>,
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
        }
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
        self
    }

    /// Derive the token budget from the model's context window (fetched
    /// per model from OpenRouter `/models`): window minus output reserve
    /// minus compaction headroom. `None` (window unknown) keeps the
    /// default; a `SLAG_CHAR_BUDGET` override always wins.
    pub fn with_context_window(mut self, window_tokens: Option<u64>) -> Self {
        if env_token_budget().is_none() {
            if let Some(window) = window_tokens {
                self.token_budget = budget_for_window(window);
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

    pub fn with_cancel(mut self, f: CancelFlag) -> Self {
        self.cancel = Some(f);
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

        for turn in 1..=self.max_turns {
            self.check_cancel()?;
            let steered = applied_steers.len();
            self.apply_steers(&mut messages, applied_steers);
            if applied_steers.len() > steered {
                // Steers rewrite an existing tool result in place: the
                // append-only journal can only stay true via a redump.
                record_rewrite(tw, &messages);
            }
            if compact_to_tokens(&mut messages, token_budget, &mut anchor) {
                record_rewrite(tw, &messages);
            }
            emit(&self.events, EngineEvent::TurnStart { turn });
            emit(&self.events, EngineEvent::ModelCall { model: self.model.clone() });

            let resp = match self
                .chat_shrinking(&mut messages, true, &mut token_budget, &mut anchor, &mut out_tokens, tw)
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    emit(&self.events, EngineEvent::Error { message: e.to_string() });
                    return Err(e);
                }
            };
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

            if resp.tool_calls.is_empty() {
                if resp.finish_reason == FinishReason::Length {
                    if !escalated {
                        // Silent escalation: discard the partial and retry
                        // the same request with a raised output cap — no
                        // transcript noise when the cap alone was the issue.
                        escalated = true;
                        out_tokens = Some(out_tokens.unwrap_or(0).max(RAISED_MAX_TOKENS));
                        continue;
                    }
                    if nudges < MAX_CONTINUE_NUDGES {
                        // Truncated even at the raised cap: keep the partial
                        // and nudge a direct resume.
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
                emit(&self.events, EngineEvent::Finish { summary: resp.content.clone() });
                return Ok(resp.content);
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

            let outcomes = self.dispatch_batch(&calls).await;

            // Every tool_call_id gets a tool result, errors included — the
            // API never sees a dangling id (hermes pattern #7).
            let mut finish_summary: Option<String> = None;
            for (call, outcome) in calls.iter().zip(&outcomes) {
                let content = if outcome.is_error {
                    format!("ERROR: {}", outcome.output)
                } else {
                    outcome.output.clone()
                };
                messages.push(ChatMessage::tool_result(&call.id, content));
                record_msg(tw, messages.last().unwrap());
                if call.name == "finish" && !outcome.is_error && finish_summary.is_none() {
                    finish_summary = Some(outcome.output.clone());
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
        messages.push(ChatMessage::user(
            "no more tool budget — summarize what was done",
        ));
        record_msg(tw, messages.last().unwrap());
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
        emit(&self.events, EngineEvent::Finish { summary: resp.content.clone() });
        Ok(resp.content)
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
        self.cancel.as_ref().is_some_and(|f| f.load(Ordering::SeqCst))
    }

    /// Hard interrupt: checked at each turn boundary, before the model call.
    fn check_cancel(&self) -> Result<(), SlagError> {
        if self.is_cancelled() {
            let e = SlagError::Cancelled;
            emit(&self.events, EngineEvent::Error { message: e.to_string() });
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
                            if out_tokens.map_or(true, |t| cap < t) {
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
            match messages.iter_mut().rev().find(|m| m.role == "tool") {
                Some(m) => {
                    m.content.push_str(&format!("\n\n{STEER_TAG}\n{text}"));
                }
                None => messages.push(ChatMessage::user(format!("{STEER_TAG}\n{text}"))),
            }
            applied.push(text);
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
            tools: if with_tools {
                ToolBox::all_specs()
            } else {
                Vec::new()
            },
            effort: self.effort,
            max_tokens,
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
                            preview: preview(&calls[i].arguments, PREVIEW_LEN),
                        },
                    );
                    (i, self.spawn_call(&calls[i]))
                })
                .collect();

            for (i, handle) in handles {
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
        tokio::spawn(async move { toolbox.dispatch(&call).await })
    }
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
    use super::*;
    use crate::engine::NormalizedResponse;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
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
        assert_eq!(reinject_file_cap(DEFAULT_TOKEN_BUDGET), REINJECT_FILE_CAP);
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
        assert!(steer_msg.content.starts_with(STEER_TAG));

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
        let first = msgs.iter().position(|m| m.content.ends_with("\nfirst")).unwrap();
        let second = msgs.iter().position(|m| m.content.ends_with("\nsecond")).unwrap();
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
        assert_eq!(sized.token_budget, budget_for_window(131_072));
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
