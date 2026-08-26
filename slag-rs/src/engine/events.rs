//! events — engine event plumbing: channel, JSONL sink, stderr narrator.
//!
//! One typed event stream (`EngineEvent` in `engine::mod`) feeds three
//! consumers: the JSONL log sink in `logs/`, the compact stderr narrator
//! (stream-mode display until the Ratatui dashboard lands), and a future
//! `--json` print mode. Display must never kill the forge: every IO error
//! here is warned once and then swallowed.

use std::collections::VecDeque;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use crossterm::style::Color;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use super::{EngineEvent, EventTx, Usage};
use crate::tui::{dim, paint, spinner_frame, BRIGHT, COLD, HOT, PURE, WARM};

/// Create the engine event channel. Sender clones cheaply into the agent
/// loop; the receiver goes to exactly one sink (`spawn_jsonl_sink` or
/// `StderrNarrator::spawn_narrator`).
pub fn channel() -> (EventTx, UnboundedReceiver<EngineEvent>) {
    tokio::sync::mpsc::unbounded_channel()
}

/// Single-line, char-boundary-safe truncation with ellipsis. Collapses
/// whitespace runs (including newlines) to single spaces so tool previews
/// stay one-liners in both the narrator and the JSONL stream.
/// The readable form of a tool call's arguments for one feed line.
///
/// The raw JSON spends its first characters on a wrapper that says what
/// the tool name already said (`read_file: {"path": "src/main.ts"}`), and
/// a truncated line then breaks mid-escape. Each tool has one argument a
/// reader actually wants; this pulls it and drops the punctuation. An
/// unknown tool, or an argument shape that does not parse, keeps the raw
/// JSON -- a wrong guess would hide the very thing being debugged.
pub fn arg_summary(name: &str, arguments: &str) -> String {
    let primary = match name {
        "bash" => "command",
        "read_file" | "write_file" | "edit_file" => "path",
        "glob" | "grep" => "pattern",
        _ => return arguments.to_string(),
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str(arguments) else {
        return arguments.to_string();
    };
    match map.get(primary).and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => match (name, map.get("path").and_then(|p| p.as_str())) {
            // A grep reads as a pattern somewhere: the path is half the
            // question, and it is cheap to keep on the same line.
            ("grep", Some(path)) if !path.is_empty() => format!("{v}  in {path}"),
            _ => v.to_string(),
        },
        _ => arguments.to_string(),
    }
}

pub fn preview(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }

    let mut line = String::with_capacity(s.len().min(max * 4));
    let mut last_space = true; // leading whitespace drops
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_space {
                line.push(' ');
                last_space = true;
            }
        } else if ch.is_control() {
            // Drop ESC and every other control char so tool output can
            // never inject ANSI sequences into the terminal or dashboard.
        } else {
            line.push(ch);
            last_space = false;
        }
    }
    let line = line.trim_end().to_string();

    if line.chars().count() <= max {
        return line;
    }
    let cut: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Spawn a task that appends one serde_json line per event to `path`,
/// flushing per line. Parent directories are created. IO errors are
/// warned to stderr once, then swallowed — the forge keeps running.
pub fn spawn_jsonl_sink(
    mut rx: UnboundedReceiver<EngineEvent>,
    path: PathBuf,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut warned = false;
        let mut file = open_append(&path, &mut warned).await;

        while let Some(event) = rx.recv().await {
            let Some(f) = file.as_mut() else {
                continue; // sink is dead; drain quietly
            };
            let line = match serde_json::to_string(&event) {
                Ok(line) => line,
                Err(e) => {
                    warn_once(&mut warned, &path, &e.to_string());
                    continue;
                }
            };
            let write = async {
                f.write_all(line.as_bytes()).await?;
                f.write_all(b"\n").await?;
                f.flush().await
            };
            if let Err(e) = write.await {
                warn_once(&mut warned, &path, &e.to_string());
                file = None;
            }
        }
    })
}

async fn open_append(path: &PathBuf, warned: &mut bool) -> Option<tokio::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                warn_once(warned, path, &e.to_string());
                return None;
            }
        }
    }
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        Ok(f) => Some(f),
        Err(e) => {
            warn_once(warned, path, &e.to_string());
            None
        }
    }
}

fn warn_once(warned: &mut bool, path: &PathBuf, err: &str) {
    if !*warned {
        eprintln!("slag: event sink {} failed: {err}", path.display());
        *warned = true;
    }
}

// ─── Stream-mode card renderer ──────────────────────────────────────────
//
// The narrator is a pure state machine (`RenderState`) that turns the
// event stream into `RenderOp`s, plus a thin printer that applies them to
// stderr. The split keeps every rendering decision unit-testable without
// a terminal.

/// One display instruction produced by `RenderState::feed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderOp {
    /// A finished line; the printer appends a newline.
    Print(String),
    /// Rewrite the in-place live line: `\r` + text + clear-to-EOL. Only
    /// emitted when the state machine was built with `tty = true`.
    RewriteLive(String),
    /// Erase the live line and return the cursor to column 0.
    ClearLive,
}

/// Per-ingot accumulation feeding the IngotDone footer
/// (`✅ [i3] forged · 42s · 118k tok · $0.31`). Reset on IngotStart.
#[derive(Debug, Clone)]
pub struct IngotAccum {
    pub tokens: u64,
    pub cost: Option<f64>,
    pub started: Instant,
    pub ctx: Option<u8>,
}

impl IngotAccum {
    fn new() -> Self {
        Self { tokens: 0, cost: None, started: Instant::now(), ctx: None }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn add(&mut self, usage: &Usage) {
        self.tokens += usage.total_tokens;
        if let Some(c) = usage.cost {
            *self.cost.get_or_insert(0.0) += c;
        }
    }

    /// The ` · 42s · 118k tok · $0.31` tail.
    fn tail(&self) -> String {
        let mut s = format!(
            " · {}s · {} tok",
            self.started.elapsed().as_secs(),
            fmt_tokens(self.tokens)
        );
        if let Some(cost) = self.cost {
            s.push_str(&format!(" · ${cost:.2}"));
        }
        // A percent on every footer of a short run is noise. The number
        // only becomes actionable once the next turn may start pruning
        // history, so it stays hidden until the gauge goes hot.
        if let Some(pct) = self.ctx.filter(|p| *p >= CTX_LOUD) {
            s.push_str(&format!(" · ctx {pct}%"));
        }
        s
    }
}

/// Context fill at which the headless narrator starts printing the gauge.
/// Matches the dashboard's COLD→HOT boundary so both surfaces go loud on
/// the same turn.
const CTX_LOUD: u8 = 66;

/// `118234` → `118k`; small counts stay exact so a 3-token probe does not
/// render as `0k`.
fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{}k", (n + 500) / 1000)
    }
}

/// Tool verb + subject, Claude Code style: `Run(cargo)`, `Read(src/x.rs)`.
fn classify(name: &str, args: &str) -> (String, Option<String>) {
    let (verb, key) = match name {
        "bash" => ("Run", Some("command")),
        "read_file" => ("Read", Some("path")),
        "write_file" => ("Write", Some("path")),
        "edit_file" => ("Update", Some("path")),
        "grep" | "glob" => ("Search", Some("pattern")),
        "recipe_view" => ("Recipe", Some("name")),
        "finish" => ("Done", None),
        other => (other, None),
    };
    let arg = key.and_then(|k| extract_str_field(args, k)).map(|v| {
        if name == "bash" {
            v.split_whitespace().next().unwrap_or(&v).to_string()
        } else {
            v
        }
    });
    // A named field that failed to parse falls back to the raw preview, so
    // the card never shows an empty subject for a tool that has one.
    let arg = match (key, arg) {
        (Some(_), Some(v)) => Some(v),
        (Some(_), None) if !args.trim().is_empty() => Some(preview(args, 40)),
        _ => None,
    };
    (verb.to_string(), arg)
}

/// Pull `"key": "value"` out of an argument preview that is JSON in
/// spirit but possibly truncated mid-string (`preview` caps it at 80
/// chars with a trailing `…`). Proper JSON parses first; the scanner
/// tolerates everything else and reads up to the cut point.
fn extract_str_field(json_ish: &str, key: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_ish) {
        if let Some(s) = v.get(key).and_then(|s| s.as_str()) {
            let s = s.trim();
            return (!s.is_empty()).then(|| s.to_string());
        }
    }

    let needle = format!("\"{key}\"");
    let at = json_ish.find(&needle)?;
    let rest = json_ish[at + needle.len()..].trim_start().strip_prefix(':')?;
    let rest = rest.trim_start().strip_prefix('"')?;

    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' | '…' => break, // closing quote, or preview truncation
            '\\' => match chars.next() {
                Some('n') | Some('t') => out.push(' '),
                Some(esc) => out.push(esc),
                None => break,
            },
            _ => out.push(ch),
        }
    }
    let out = out.trim().to_string();
    (!out.is_empty()).then_some(out)
}

/// Collapsed-streak summary: `Ran 4 commands`, `Read 3 files`.
fn streak_summary(verb: &str, n: usize) -> String {
    match verb {
        "Run" => format!("Ran {n} commands"),
        "Read" => format!("Read {n} files"),
        "Write" => format!("Wrote {n} files"),
        "Update" => format!("Updated {n} files"),
        "Search" => format!("Ran {n} searches"),
        "Recipe" => format!("Viewed {n} recipes"),
        _ => format!("{n} × {verb}"),
    }
}

fn line(color: Color, icon: &str, msg: &str) -> String {
    format!("  {} {msg}", paint(color, icon))
}

/// An emitted ToolCallStart waiting for its ToolResult.
#[derive(Debug, Clone)]
struct PendingCall {
    name: String,
    verb: String,
    arg: Option<String>,
    started: Instant,
}

impl PendingCall {
    fn label(&self) -> String {
        match &self.arg {
            Some(arg) => format!("{}({arg})", self.verb),
            None => self.verb.clone(),
        }
    }
}

/// A rendered tool card: header + result fold, held briefly by the
/// batch-collapse logic before printing. `ok` exempts failures from
/// collapsing — a ✗ card must always print with its error fold.
#[derive(Debug, Clone)]
struct Card {
    verb: String,
    ok: bool,
    header: String,
    fold: String,
}

/// Stateful card renderer: pairs tool calls with results, drives the
/// live line, collapses streaks, accumulates per-ingot totals. Pure —
/// it never touches a terminal; `apply_ops` does.
pub struct RenderState {
    tty: bool,
    /// Terminal columns for live-line clamping; 0 = unclamped. A live line
    /// longer than the terminal wraps, and `\r` + clear-to-EOL then only
    /// reach the last physical row — the wrapped first row would be left
    /// behind as a permanent junk line.
    width: usize,
    pending: VecDeque<PendingCall>,
    live_shown: bool,
    spinner_idx: usize,
    /// Latest ◈ narration; feeds the live line while a call is unresolved.
    narrate: Option<String>,
    /// Routed model to show (once) as a dim suffix on the next card.
    routed_pending: Option<String>,
    last_routed: Option<String>,
    accum: IngotAccum,
    streak_verb: Option<String>,
    streak_count: usize,
    /// Second card of a possible streak, held until we know whether the
    /// third arrives (3+ collapse) or the streak breaks (print it).
    held: Option<Card>,
}

impl RenderState {
    pub fn new(tty: bool) -> Self {
        let width = if tty {
            crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(0)
        } else {
            0
        };
        Self {
            tty,
            width,
            pending: VecDeque::new(),
            live_shown: false,
            spinner_idx: 0,
            narrate: None,
            routed_pending: None,
            last_routed: None,
            accum: IngotAccum::new(),
            streak_verb: None,
            streak_count: 0,
            held: None,
        }
    }

    /// Override the live-line width clamp (tests; `new` asks the terminal).
    pub fn with_live_width(mut self, width: usize) -> Self {
        self.width = width;
        self
    }

    /// Session totals for the current ingot.
    pub fn accum(&self) -> &IngotAccum {
        &self.accum
    }

    pub fn feed(&mut self, event: &EngineEvent) -> Vec<RenderOp> {
        let mut prints: Vec<String> = Vec::new();
        match event {
            // Demoted to live-line metadata: no own lines.
            EngineEvent::TurnStart { .. } | EngineEvent::ModelCall { .. } => {}
            // The gauge rides the ingot footer rather than claiming a line
            // of its own; the JSONL sink still records every reading.
            EngineEvent::ContextGauge { pct, .. } => self.accum.ctx = Some(*pct),
            EngineEvent::ModelRouted { routed, .. } => {
                if self.last_routed.as_deref() != Some(routed) {
                    self.last_routed = Some(routed.clone());
                    self.routed_pending = Some(routed.clone());
                }
            }
            EngineEvent::Tokens { usage } => {
                self.accum.add(usage);
            }

            EngineEvent::ToolCallStart { name, preview: p } => {
                let (verb, arg) = classify(name, p);
                self.pending.push_back(PendingCall {
                    name: name.clone(),
                    verb,
                    arg,
                    started: Instant::now(),
                });
            }
            EngineEvent::ToolResult { name, ok, preview: p, .. } => {
                let card = self.build_card(name, *ok, p);
                self.push_card(card, &mut prints);
                self.narrate = None;
            }

            EngineEvent::Narrate { text } => {
                if self.tty && !self.pending.is_empty() {
                    self.narrate = Some(preview(text, 90));
                } else {
                    self.flush_streak(&mut prints);
                    prints.push(line(COLD, "◈", &dim(&preview(text, 110))));
                }
            }

            // Kept lines. Each breaks any card streak so order stays true.
            EngineEvent::Steer { text } => {
                self.flush_streak(&mut prints);
                prints.push(line(BRIGHT, "↪", &format!("steer: {}", preview(text, 80))));
            }
            EngineEvent::Finish { summary } => {
                self.flush_streak(&mut prints);
                prints.push(line(PURE, "■", &preview(summary, 120)));
            }
            EngineEvent::Error { message } => {
                self.flush_streak(&mut prints);
                prints.push(line(WARM, "✗", &preview(message, 120)));
            }
            EngineEvent::Warning { message } => {
                self.flush_streak(&mut prints);
                prints.push(line(BRIGHT, "⚠", &preview(message, 110)));
            }
            EngineEvent::ApiRetry { attempt, status, remaining_secs } => {
                self.flush_streak(&mut prints);
                prints.push(line(
                    BRIGHT,
                    "⏳",
                    &dim(&format!("api retry {attempt} · {status} · {remaining_secs}s left")),
                ));
            }
            EngineEvent::IngotStart { id, work } => {
                self.flush_streak(&mut prints);
                self.accum.reset();
                prints.push(line(HOT, "🧱", &format!("[{id}] {}", preview(work, 60))));
            }
            EngineEvent::HeatTick { id, heat } => {
                self.flush_streak(&mut prints);
                prints.push(line(WARM, "🔥", &format!("[{id}] heat {heat}")));
            }
            EngineEvent::IngotDone { id, ok } => {
                self.flush_streak(&mut prints);
                let (color, icon, word) =
                    if *ok { (PURE, "✅", "forged") } else { (WARM, "❌", "cracked") };
                prints.push(line(
                    color,
                    icon,
                    &format!("[{id}] {word}{}", dim(&self.accum.tail())),
                ));
            }
            EngineEvent::DuelRound { id, round } => {
                self.flush_streak(&mut prints);
                prints.push(line(BRIGHT, "⚔", &format!("[{id}] duel round {round}")));
            }
            EngineEvent::DuelVerdict { id, winner, margin } => {
                self.flush_streak(&mut prints);
                prints.push(line(PURE, "⚖", &format!("[{id}] cast {winner} wins by {margin}")));
            }
            // A hook's start is JSONL-only: the finish line carries the
            // verdict and the duration, and printing both would double
            // every formatter hook in the stream.
            EngineEvent::HookStarted { .. } => {}
            EngineEvent::HookFinished { name, code, duration_ms, .. } => {
                // Successful hooks are background noise; a block or a
                // failure is news the operator needs.
                if *code != 0 {
                    self.flush_streak(&mut prints);
                    let verdict = if *code == 2 { "blocked" } else { "failed" };
                    prints.push(line(BRIGHT, "⚓", &format!("hook {name} {verdict} ({duration_ms}ms)")));
                }
            }
        }
        self.assemble(prints)
    }

    /// End of stream: print anything held and drop the live line.
    pub fn finish(&mut self) -> Vec<RenderOp> {
        let mut prints = Vec::new();
        self.flush_streak(&mut prints);
        self.pending.clear();
        self.assemble(prints)
    }

    /// Wrap this step's prints with live-line bookkeeping: clear the old
    /// live line before printing over it, re-render it after when a call
    /// is still unresolved.
    fn assemble(&mut self, prints: Vec<String>) -> Vec<RenderOp> {
        let live_next = self.tty && !self.pending.is_empty();
        let mut ops = Vec::new();
        if self.live_shown && (!prints.is_empty() || !live_next) {
            ops.push(RenderOp::ClearLive);
            self.live_shown = false;
        }
        ops.extend(prints.into_iter().map(RenderOp::Print));
        if live_next {
            self.spinner_idx = self.spinner_idx.wrapping_add(1);
            ops.push(RenderOp::RewriteLive(self.live_text()));
            self.live_shown = true;
        }
        ops
    }

    /// Spinner + narration (or card header) + elapsed + cumulative spend,
    /// clamped to one terminal row so `\r` rewrites never leave a wrapped
    /// first row behind.
    fn live_text(&self) -> String {
        // Label and elapsed must describe the SAME call — the oldest
        // unresolved one — or the line pairs a fresh call's name with a
        // stale call's timer.
        let mut label = self
            .narrate
            .clone()
            .or_else(|| self.pending.front().map(PendingCall::label))
            .unwrap_or_default();
        let secs = self
            .pending
            .front()
            .map(|c| c.started.elapsed().as_secs())
            .unwrap_or(0);
        let mut meta = format!(" · {secs}s · {} tok", fmt_tokens(self.accum.tokens));
        if let Some(cost) = self.accum.cost {
            meta.push_str(&format!(" · ${cost:.2}"));
        }
        if self.width > 0 {
            // Visible budget: one row minus the last column (writing into
            // it would trigger autowrap on most terminals).
            let budget = self.width.saturating_sub(1);
            let fixed = 4; // "  " indent + spinner frame + space
            let label_budget = budget.saturating_sub(fixed + meta.chars().count());
            if label.chars().count() > label_budget {
                label = preview(&label, label_budget);
            }
            if label.is_empty() && fixed + meta.chars().count() > budget {
                meta = meta.chars().take(budget.saturating_sub(fixed)).collect();
            }
        }
        format!(
            "  {} {}{}",
            paint(HOT, spinner_frame(self.spinner_idx)),
            paint(PURE, &label),
            dim(&meta)
        )
    }

    /// Sequential pairing: a result claims the oldest unresolved call with
    /// the same tool name (falling back to the front of the queue, so one
    /// mismatch cannot wedge the pairing forever).
    fn take_pending(&mut self, name: &str) -> Option<PendingCall> {
        match self.pending.iter().position(|c| c.name == name) {
            Some(at) => self.pending.remove(at),
            None => self.pending.pop_front(),
        }
    }

    fn build_card(&mut self, name: &str, ok: bool, result_preview: &str) -> Card {
        let (verb, label) = match self.take_pending(name) {
            Some(call) => (call.verb.clone(), call.label()),
            // Unmatched result (start line lost): render from the name.
            None => {
                let (verb, _) = classify(name, "");
                (verb.clone(), verb)
            }
        };
        let mut header = format!("  {} {}", paint(HOT, "⏺"), paint(PURE, &label));
        if let Some(routed) = self.routed_pending.take() {
            header.push_str(&dim(&format!(" · {routed}")));
        }
        let fold = if ok {
            format!("    {}", dim("└ ok"))
        } else {
            let first = result_preview.lines().next().unwrap_or("");
            format!(
                "    {} {}",
                dim("└"),
                paint(WARM, &format!("✗ {}", preview(first, 80)))
            )
        };
        Card { verb, ok, header, fold }
    }

    /// Streak-aware card emission. First card of a verb prints at once;
    /// the second is held; from the third on the streak is collapsing and
    /// resolves to one summary line when it breaks. Failures never join a
    /// streak: their card and error fold always print, so an unattended
    /// forge can never summarize failing commands into an all-green line.
    fn push_card(&mut self, card: Card, prints: &mut Vec<String>) {
        if !card.ok {
            self.flush_streak(prints);
            prints.push(card.header);
            prints.push(card.fold);
            return;
        }
        if self.streak_verb.as_deref() == Some(card.verb.as_str()) {
            self.streak_count += 1;
            if self.streak_count == 2 {
                self.held = Some(card);
            } else {
                self.held = None; // 3+: the held second card collapses too
            }
        } else {
            self.flush_streak(prints);
            self.streak_verb = Some(card.verb.clone());
            self.streak_count = 1;
            prints.push(card.header);
            prints.push(card.fold);
        }
    }

    fn flush_streak(&mut self, prints: &mut Vec<String>) {
        if self.streak_count >= 3 {
            let verb = self.streak_verb.as_deref().unwrap_or_default();
            prints.push(format!(
                "  {} {}",
                paint(HOT, "⏺"),
                dim(&streak_summary(verb, self.streak_count))
            ));
        } else if let Some(held) = self.held.take() {
            prints.push(held.header);
            prints.push(held.fold);
        }
        self.held = None;
        self.streak_verb = None;
        self.streak_count = 0;
    }
}

/// Cross-narrator live-line state. Parallel anvils each spawn their own
/// narrator (one per smith invocation), and every `RenderState` only knows
/// about its *own* live line — without shared state, narrator B's Print
/// lands at the cursor parked mid-way through narrator A's un-terminated
/// live line and A's next `\r` rewrite overwrites B's card. The mutex
/// serializes stderr writes; the flag records whether ANY narrator holds
/// an un-terminated live line so the next writer clears it first.
static STDERR_LIVE: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// Apply render ops to stderr. The only place the narrator touches a
/// terminal; IO errors are swallowed (display must never kill the forge).
fn apply_ops(ops: &[RenderOp]) {
    let mut live = STDERR_LIVE.lock().unwrap_or_else(|p| p.into_inner());
    let mut err = std::io::stderr().lock();
    write_ops(ops, &mut err, &mut live);
    let _ = err.flush();
}

/// Testable core of `apply_ops`: `live` is the shared "an un-terminated
/// live line occupies the current row" flag.
fn write_ops(ops: &[RenderOp], w: &mut impl Write, live: &mut bool) {
    for op in ops {
        let _ = match op {
            RenderOp::Print(text) => {
                let clear = if *live { write!(w, "\r\x1b[K") } else { Ok(()) };
                *live = false;
                clear.and_then(|()| writeln!(w, "{text}"))
            }
            RenderOp::RewriteLive(text) => {
                *live = true;
                write!(w, "\r{text}\x1b[K")
            }
            RenderOp::ClearLive => {
                *live = false;
                write!(w, "\r\x1b[K")
            }
        };
    }
}

/// Stateful card display on stderr — the stream-mode view of the agent
/// loop until the Ratatui dashboard lands.
pub struct StderrNarrator;

impl StderrNarrator {
    pub fn spawn_narrator(mut rx: UnboundedReceiver<EngineEvent>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut state = RenderState::new(std::io::stderr().is_terminal());
            while let Some(event) = rx.recv().await {
                apply_ops(&state.feed(&event));
            }
            apply_ops(&state.finish());
        })
    }
}

/// Pipeline-level print path: route a finished line (ingot header, footer,
/// status) through the shared stderr live-line state instead of a bare
/// `println!`. A narrator's parked live line is cleared first, so a
/// spinner row is never orphaned above an ingot footer.
pub fn print_line(text: impl Into<String>) {
    apply_ops(&[RenderOp::Print(text.into())]);
}

// ─── Run log (item 81) ──────────────────────────────────────────────────
//
// One self-describing JSONL file per forge run: typed metadata at the
// top (run id, git branch, model, crucible fingerprint), ingot outcomes
// as they land, and the assay verdict at the bottom — a runs lister
// needs no sidecar files. Distinct from the per-session engine-*.jsonl
// event streams: this is the run's ledger, not its firehose.

/// One typed line in the run log. Deserialize so `slag insights` can
/// read the ledgers back; unknown future fields are tolerated per-line
/// by the crash-tolerant reader.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum RunEntry {
    /// First line of every run log.
    RunMeta {
        run_id: String,
        started: String,
        git_branch: Option<String>,
        model: String,
        duel: String,
        /// Which flux inputs the smith was fed at forge start
        /// (`flux::profile`), so two runs on one model stay distinguishable.
        flux_profile: String,
        /// FNV fingerprint of the crucible (PLAN.md) at forge start, so
        /// a lister can tell which runs forged the same plan.
        crucible_hash: Option<String>,
    },
    /// One ingot left the forge.
    IngotDone { id: String, ok: bool, heat: u8 },
    /// Free-form note (budget pauses and the like).
    Note { message: String },
    /// Last line: the assay verdict.
    Assay { total: usize, forged: usize, cracked: usize, ok: bool },
}

/// Append-only run log writer. Synchronous and best-effort: entries are
/// rare (per-ingot, not per-token) and logging must never kill the forge.
pub struct RunLog {
    path: PathBuf,
    warned: std::sync::Mutex<bool>,
}

impl RunLog {
    /// Create `logs/run-<run_id>.jsonl` under `dir` and write the meta
    /// entry as its first line.
    pub fn create(dir: &std::path::Path, run_id: &str, meta: RunEntry) -> Self {
        let log = Self {
            path: dir.join(format!("run-{run_id}.jsonl")),
            warned: std::sync::Mutex::new(false),
        };
        log.append(&meta);
        log
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn append(&self, entry: &RunEntry) {
        let line = match serde_json::to_string(entry) {
            Ok(line) => line,
            Err(e) => return self.warn(&e.to_string()),
        };
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let write = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| {
                f.write_all(line.as_bytes())?;
                f.write_all(b"\n")
            });
        if let Err(e) = write {
            self.warn(&e.to_string());
        }
    }

    fn warn(&self, err: &str) {
        let mut warned = self.warned.lock().unwrap_or_else(|p| p.into_inner());
        if !*warned {
            eprintln!("slag: run log {} failed: {err}", self.path.display());
            *warned = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Usage;

    #[test]
    fn events_serialize_with_snake_case_tags() {
        let v = serde_json::to_value(EngineEvent::TurnStart { turn: 3 }).unwrap();
        assert_eq!(v, serde_json::json!({"event": "turn_start", "turn": 3}));

        let v = serde_json::to_value(EngineEvent::ToolCallStart {
            name: "bash".into(),
            preview: "cargo test".into(),
        })
        .unwrap();
        assert_eq!(v["event"], "tool_call_start");
        assert_eq!(v["name"], "bash");

        let v = serde_json::to_value(EngineEvent::ToolResult {
            name: "edit_file".into(),
            ok: false,
            preview: "no match".into(),
            lines: 1,
            bytes: 8,
            ms: 12,
        })
        .unwrap();
        assert_eq!(v["event"], "tool_result");
        assert_eq!(v["ok"], false);

        let v = serde_json::to_value(EngineEvent::Finish { summary: "done".into() }).unwrap();
        assert_eq!(v["event"], "finish");
    }

    #[test]
    fn channel_delivers_events() {
        let (tx, mut rx) = channel();
        tx.send(EngineEvent::TurnStart { turn: 1 }).unwrap();
        drop(tx);
        assert!(matches!(rx.try_recv(), Ok(EngineEvent::TurnStart { turn: 1 })));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn jsonl_sink_writes_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path also proves parent-dir creation.
        let path = dir.path().join("logs").join("events.jsonl");

        let (tx, rx) = channel();
        let handle = spawn_jsonl_sink(rx, path.clone());

        tx.send(EngineEvent::TurnStart { turn: 1 }).unwrap();
        tx.send(EngineEvent::ModelCall { model: "qwen/qwen3-coder".into() }).unwrap();
        tx.send(EngineEvent::Tokens { usage: Usage::default() }).unwrap();
        drop(tx);
        handle.await.unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v["event"].is_string());
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[1]).unwrap()["model"],
            "qwen/qwen3-coder"
        );
    }

    #[tokio::test]
    async fn jsonl_sink_survives_unwritable_path() {
        let (tx, rx) = channel();
        let handle = spawn_jsonl_sink(rx, PathBuf::from("/dev/null/impossible/events.jsonl"));
        tx.send(EngineEvent::TurnStart { turn: 1 }).unwrap();
        drop(tx);
        // Must complete without panicking despite the doomed path.
        handle.await.unwrap();
    }

    #[test]
    fn arg_summary_pulls_the_argument_a_reader_wants() {
        assert_eq!(arg_summary("read_file", r#"{"path": "src/main.ts"}"#), "src/main.ts");
        assert_eq!(arg_summary("bash", r#"{"command": "npx tsc --noEmit"}"#), "npx tsc --noEmit");
        assert_eq!(arg_summary("glob", r#"{"pattern": "**/*.rs"}"#), "**/*.rs");
    }

    #[test]
    fn arg_summary_keeps_a_greps_path_beside_its_pattern() {
        assert_eq!(
            arg_summary("grep", r#"{"pattern": "TODO", "path": "src"}"#),
            "TODO  in src"
        );
    }

    #[test]
    fn arg_summary_falls_back_to_raw_json_rather_than_guessing() {
        // An unknown tool, a shape that does not parse, and a missing
        // field all keep the raw arguments: hiding them would hide the
        // one thing a reader is debugging.
        let odd = r#"{"whatever": 1}"#;
        assert_eq!(arg_summary("mcp__x__y", odd), odd);
        assert_eq!(arg_summary("read_file", "not json"), "not json");
        assert_eq!(arg_summary("bash", r#"{"command": ""}"#), r#"{"command": ""}"#);
    }

    #[test]
    fn preview_truncates_on_char_boundary_with_multibyte() {
        let s = "héllo wörld ⚒⚒⚒ fire";
        let p = preview(s, 8);
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= 8);
        // Never splits a multibyte char: the result is valid UTF-8 by
        // construction, and re-slicing must not panic.
        let _ = &p[..];

        let all_multibyte = "⚒⚒⚒⚒⚒⚒";
        let p = preview(all_multibyte, 4);
        assert_eq!(p.chars().count(), 4);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn preview_collapses_whitespace_and_passes_short_input() {
        assert_eq!(preview("cargo test", 80), "cargo test");
        assert_eq!(preview("  cargo\n\ttest \n --all ", 80), "cargo test --all");
        assert_eq!(preview("anything", 0), "");
        // Exactly at the limit: no ellipsis.
        assert_eq!(preview("abcd", 4), "abcd");
    }

    #[test]
    fn preview_strips_control_chars() {
        // ANSI color/clear sequences lose their ESC and cannot re-arm.
        assert_eq!(preview("a\x1b[31mred\x1b[0m b", 80), "a[31mred[0m b");
        assert_eq!(preview("bell\x07 and \x08backspace", 80), "bell and backspace");
        // Whitespace controls (\n, \t) still collapse to single spaces.
        assert_eq!(preview("x\n\ty", 80), "x y");
    }

    // ─── RenderState (stream-mode card renderer) ───

    fn start(name: &str, args: &str) -> EngineEvent {
        EngineEvent::ToolCallStart { name: name.into(), preview: args.into() }
    }

    fn result(name: &str, ok: bool, p: &str) -> EngineEvent {
        EngineEvent::ToolResult {
            name: name.into(),
            ok,
            preview: p.into(),
            lines: p.lines().count(),
            bytes: p.len(),
            ms: 0,
        }
    }

    fn printed(ops: &[RenderOp]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                RenderOp::Print(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn feed_all(state: &mut RenderState, events: &[EngineEvent]) -> Vec<RenderOp> {
        let mut ops: Vec<RenderOp> = events.iter().flat_map(|e| state.feed(e)).collect();
        ops.extend(state.finish());
        ops
    }

    #[test]
    fn pairing_renders_card_header_and_result_fold() {
        let mut state = RenderState::new(false);
        assert!(state.feed(&start("bash", r#"{"command": "cargo test --all"}"#)).is_empty());

        let ops = state.feed(&result("bash", true, "221 passed"));
        let lines = printed(&ops);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains('⏺') && lines[0].contains("Run(cargo)"), "{}", lines[0]);
        assert!(lines[1].contains("└ ok"), "{}", lines[1]);
    }

    #[test]
    fn failed_result_folds_first_error_line() {
        let mut state = RenderState::new(false);
        state.feed(&start("edit_file", r#"{"path": "src/x.rs"}"#));
        let ops = state.feed(&result("edit_file", false, "no match for old_string"));
        let lines = printed(&ops);
        assert!(lines[0].contains("Update(src/x.rs)"), "{}", lines[0]);
        assert!(lines[1].contains('└') && lines[1].contains("✗ no match for old_string"));
    }

    #[test]
    fn classify_maps_verbs_and_extracts_subjects() {
        assert_eq!(
            classify("bash", r#"{"command": "cargo test --all"}"#),
            ("Run".into(), Some("cargo".into()))
        );
        assert_eq!(
            classify("read_file", r#"{"path": "src/main.rs"}"#),
            ("Read".into(), Some("src/main.rs".into()))
        );
        assert_eq!(
            classify("write_file", r#"{"path": "a.txt", "content": "hi"}"#),
            ("Write".into(), Some("a.txt".into()))
        );
        assert_eq!(
            classify("grep", r#"{"pattern": "fn main", "path": "."}"#),
            ("Search".into(), Some("fn main".into()))
        );
        assert_eq!(
            classify("recipe_view", r#"{"name": "deploy"}"#),
            ("Recipe".into(), Some("deploy".into()))
        );
        assert_eq!(classify("finish", r#"{"summary": "done"}"#), ("Done".into(), None));
        // Unknown tools pass their name through as the verb.
        assert_eq!(classify("mystery", "{}").0, "mystery");
    }

    #[test]
    fn classify_tolerates_truncated_json_previews() {
        // `preview` cuts at 80 chars and appends `…` — mid-string.
        let (verb, arg) = classify("write_file", r#"{"path": "src/core/rng.ts", "content": "export function ra…"#);
        assert_eq!(verb, "Write");
        assert_eq!(arg.as_deref(), Some("src/core/rng.ts"));

        // Truncated inside the value itself: take what survived the cut.
        let (verb, arg) = classify("read_file", r#"{"path": "src/very/deep/mo…"#);
        assert_eq!(verb, "Read");
        assert_eq!(arg.as_deref(), Some("src/very/deep/mo"));

        // Unparseable garbage: fall back to the raw preview.
        let (verb, arg) = classify("bash", "not json at all");
        assert_eq!(verb, "Run");
        assert_eq!(arg.as_deref(), Some("not json at all"));
    }

    #[test]
    fn extract_str_field_unescapes_and_rejects_empty() {
        assert_eq!(
            extract_str_field(r#"{"command": "echo \"hi\""}"#, "command").as_deref(),
            Some(r#"echo "hi""#)
        );
        assert_eq!(extract_str_field(r#"{"path": ""}"#, "path"), None);
        assert_eq!(extract_str_field("{}", "path"), None);
    }

    #[test]
    fn three_plus_same_verb_cards_collapse_to_summary() {
        let mut state = RenderState::new(false);
        let mut events = Vec::new();
        for cmd in ["ls", "pwd", "date", "whoami"] {
            events.push(start("bash", &format!(r#"{{"command": "{cmd}"}}"#)));
            events.push(result("bash", true, "ok"));
        }
        events.push(start("read_file", r#"{"path": "a.rs"}"#));
        events.push(result("read_file", true, "content"));

        let lines = printed(&feed_all(&mut state, &events));
        let joined = lines.join("\n");
        // First card prints fully; cards 2-4 collapse into the summary.
        assert!(joined.contains("Run(ls)"), "{joined}");
        for held in ["Run(pwd)", "Run(date)", "Run(whoami)"] {
            assert!(!joined.contains(held), "{held} should be collapsed\n{joined}");
        }
        assert!(joined.contains("Ran 4 commands"), "{joined}");
        // The summary lands when the streak breaks — before the Read card.
        let summary_at = lines.iter().position(|l| l.contains("Ran 4 commands")).unwrap();
        let read_at = lines.iter().position(|l| l.contains("Read(a.rs)")).unwrap();
        assert!(summary_at < read_at);
    }

    #[test]
    fn failing_cards_never_collapse_into_a_streak() {
        let mut state = RenderState::new(false);
        let mut events = Vec::new();
        for (cmd, ok) in [
            ("ls", true),
            ("cargo test", false),
            ("cargo test", false),
            ("cargo build", true),
        ] {
            events.push(start("bash", &format!(r#"{{"command": "{cmd}"}}"#)));
            events.push(result("bash", ok, if ok { "ok" } else { "2 tests failed" }));
        }
        let lines = printed(&feed_all(&mut state, &events));
        let joined = lines.join("\n");
        // Both failure folds must print — held-then-discarded or swallowed
        // failures would show an all-green summary while the smith flails.
        let fails = lines
            .iter()
            .filter(|l| l.contains("✗ 2 tests failed"))
            .count();
        assert_eq!(fails, 2, "{joined}");
        assert!(!joined.contains("Ran 4 commands"), "{joined}");
    }

    #[test]
    fn print_after_another_narrators_live_line_clears_it_first() {
        // Anvil A parks an un-terminated live line; anvil B's narrator
        // prints a card. The shared live flag must clear A's row first so
        // B's card does not land mid-line.
        let mut out: Vec<u8> = Vec::new();
        let mut live = false;
        write_ops(&[RenderOp::RewriteLive("  ◐ Run(cargo)".into())], &mut out, &mut live);
        assert!(live);
        write_ops(&[RenderOp::Print("  ⏺ Read(src/x.rs)".into())], &mut out, &mut live);
        assert!(!live);
        let s = String::from_utf8(out).unwrap();
        let print_at = s.find("⏺ Read").unwrap();
        let clear_at = s.rfind("\r\x1b[K").unwrap();
        assert!(
            clear_at < print_at,
            "print must clear the foreign live line first: {s:?}"
        );
    }

    #[test]
    fn live_line_is_clamped_to_the_terminal_width() {
        fn visible_len(s: &str) -> usize {
            // Strip ANSI CSI sequences, count chars.
            let mut n = 0;
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    n += 1;
                }
            }
            n
        }

        let mut state = RenderState::new(true).with_live_width(40);
        state.feed(&start("bash", r#"{"command": "cargo build"}"#));
        let ops = state.feed(&EngineEvent::Narrate {
            text: "compiling the crate and resolving a very long dependency graph \
                   with many transitive members"
                .into(),
        });
        let live = ops
            .iter()
            .find_map(|op| match op {
                RenderOp::RewriteLive(s) => Some(s.clone()),
                _ => None,
            })
            .expect("live line");
        assert!(
            visible_len(&live) < 40,
            "live line must fit one 40-col row, got {} chars: {live:?}",
            visible_len(&live)
        );

        // Unclamped states (width 0) keep the full narration.
        let mut wide = RenderState::new(true).with_live_width(0);
        wide.feed(&start("bash", r#"{"command": "cargo build"}"#));
        let ops = wide.feed(&EngineEvent::Narrate {
            text: "compiling the crate and resolving a very long dependency graph \
                   with many transitive members"
                .into(),
        });
        let live = ops
            .iter()
            .find_map(|op| match op {
                RenderOp::RewriteLive(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap();
        assert!(visible_len(&live) > 40, "{live:?}");
    }

    #[test]
    fn two_card_streak_does_not_collapse() {
        let mut state = RenderState::new(false);
        let events = [
            start("bash", r#"{"command": "ls"}"#),
            result("bash", true, "ok"),
            start("bash", r#"{"command": "pwd"}"#),
            result("bash", true, "ok"),
            start("read_file", r#"{"path": "a.rs"}"#),
            result("read_file", true, "content"),
        ];
        let joined = printed(&feed_all(&mut state, &events)).join("\n");
        assert!(joined.contains("Run(ls)") && joined.contains("Run(pwd)"), "{joined}");
        assert!(!joined.contains("Ran 2"), "{joined}");
    }

    #[test]
    fn finish_flushes_a_held_second_card() {
        let mut state = RenderState::new(false);
        state.feed(&start("bash", r#"{"command": "ls"}"#));
        state.feed(&result("bash", true, "ok"));
        state.feed(&start("bash", r#"{"command": "pwd"}"#));
        let mid = state.feed(&result("bash", true, "ok"));
        assert!(printed(&mid).is_empty(), "second card is held until the streak resolves");
        let joined = printed(&state.finish()).join("\n");
        assert!(joined.contains("Run(pwd)"), "{joined}");
    }

    #[test]
    fn non_tty_emits_only_plain_prints() {
        let mut state = RenderState::new(false);
        let events = [
            EngineEvent::TurnStart { turn: 1 },
            EngineEvent::ModelCall { model: "m".into() },
            start("bash", r#"{"command": "ls"}"#),
            EngineEvent::Narrate { text: "listing files".into() },
            EngineEvent::Tokens { usage: Usage { total_tokens: 42, ..Default::default() } },
            result("bash", true, "ok"),
        ];
        let ops = feed_all(&mut state, &events);
        assert!(
            ops.iter().all(|op| matches!(op, RenderOp::Print(_))),
            "non-TTY must never rewrite: {ops:?}"
        );
        // With no live line, narration prints as its own dim line.
        assert!(printed(&ops).iter().any(|l| l.contains('◈') && l.contains("listing files")));
    }

    #[test]
    fn tty_drives_a_live_line_and_clears_it_on_resolution() {
        let mut state = RenderState::new(true);

        let ops = state.feed(&start("bash", r#"{"command": "cargo build"}"#));
        assert!(matches!(&ops[..], [RenderOp::RewriteLive(live)]
            if live.contains("Run(cargo)") && live.contains("tok")));

        // Tokens update the live line in place — no printed line.
        let usage = Usage { total_tokens: 1500, cost: Some(0.02), ..Default::default() };
        let ops = state.feed(&EngineEvent::Tokens { usage });
        assert!(matches!(&ops[..], [RenderOp::RewriteLive(live)]
            if live.contains("1.5k tok") && live.contains("$0.02")));

        // Narration feeds the live line instead of printing.
        let ops = state.feed(&EngineEvent::Narrate { text: "compiling the crate".into() });
        assert!(matches!(&ops[..], [RenderOp::RewriteLive(live)]
            if live.contains("compiling the crate")));

        // Resolution clears the live line, then prints the card.
        let ops = state.feed(&result("bash", true, "ok"));
        assert_eq!(ops[0], RenderOp::ClearLive);
        assert!(!ops.iter().any(|op| matches!(op, RenderOp::RewriteLive(_))));
        assert!(printed(&ops)[0].contains("Run(cargo)"));
    }

    #[test]
    fn demoted_events_print_no_lines() {
        let mut state = RenderState::new(false);
        for event in [
            EngineEvent::TurnStart { turn: 2 },
            EngineEvent::ModelCall { model: "qwen/qwen3-coder".into() },
            EngineEvent::ModelRouted { requested: "auto".into(), routed: "deepseek/v3".into() },
            EngineEvent::Tokens { usage: Usage::default() },
        ] {
            assert!(state.feed(&event).is_empty(), "{event:?} must not print");
        }
    }

    #[test]
    fn routed_model_shows_once_as_a_suffix_on_the_next_card() {
        let mut state = RenderState::new(false);
        state.feed(&EngineEvent::ModelRouted {
            requested: "openrouter/auto".into(),
            routed: "deepseek/v3".into(),
        });

        state.feed(&start("bash", r#"{"command": "ls"}"#));
        let first = printed(&state.feed(&result("bash", true, "ok"))).join("\n");
        assert!(first.contains("deepseek/v3"), "{first}");

        // Same routing again: no news, no suffix on the following card.
        state.feed(&EngineEvent::ModelRouted {
            requested: "openrouter/auto".into(),
            routed: "deepseek/v3".into(),
        });
        state.feed(&start("grep", r#"{"pattern": "fn"}"#));
        let second = printed(&state.feed(&result("grep", true, "3 hits"))).join("\n");
        assert!(!second.contains("deepseek/v3"), "{second}");
    }

    #[test]
    fn ingot_accumulation_feeds_the_footer_and_resets() {
        let mut state = RenderState::new(false);
        state.feed(&EngineEvent::IngotStart { id: "i3".into(), work: "forge the rng".into() });
        state.feed(&EngineEvent::Tokens {
            usage: Usage { total_tokens: 118_000, cost: Some(0.31), ..Default::default() },
        });
        let done = printed(&state.feed(&EngineEvent::IngotDone { id: "i3".into(), ok: true }));
        assert!(done[0].contains("[i3] forged"), "{}", done[0]);
        assert!(done[0].contains("118k tok"), "{}", done[0]);
        assert!(done[0].contains("$0.31"), "{}", done[0]);
        assert!(done[0].contains('s'), "{}", done[0]);

        // Next ingot starts from zero — no bleed-through.
        state.feed(&EngineEvent::IngotStart { id: "i4".into(), work: "next".into() });
        let done = printed(&state.feed(&EngineEvent::IngotDone { id: "i4".into(), ok: false }));
        assert!(done[0].contains("[i4] cracked"), "{}", done[0]);
        assert!(done[0].contains("0 tok"), "{}", done[0]);
        assert!(!done[0].contains('$'), "{}", done[0]);
    }

    #[test]
    fn gauge_stays_silent_until_compaction_is_in_sight() {
        let mut state = RenderState::new(false);
        state.feed(&EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });

        // A cool gauge prints nothing of its own and leaves the footer bare.
        let quiet = printed(&state.feed(&EngineEvent::ContextGauge {
            pct: 40,
            used_tokens: 80_000,
            budget_tokens: 200_000,
        }));
        assert!(quiet.is_empty(), "{quiet:?}");
        let done = printed(&state.feed(&EngineEvent::IngotDone { id: "i1".into(), ok: true }));
        assert!(!done[0].contains("ctx"), "{}", done[0]);

        // Hot: the footer carries the reading.
        state.feed(&EngineEvent::IngotStart { id: "i2".into(), work: "w".into() });
        state.feed(&EngineEvent::ContextGauge {
            pct: 91,
            used_tokens: 182_000,
            budget_tokens: 200_000,
        });
        let done = printed(&state.feed(&EngineEvent::IngotDone { id: "i2".into(), ok: true }));
        assert!(done[0].contains("ctx 91%"), "{}", done[0]);

        // A fresh ingot forgets the previous one's reading.
        state.feed(&EngineEvent::IngotStart { id: "i3".into(), work: "w".into() });
        let done = printed(&state.feed(&EngineEvent::IngotDone { id: "i3".into(), ok: true }));
        assert!(!done[0].contains("ctx"), "{}", done[0]);
    }

    #[test]
    fn fmt_tokens_scales_readably() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1500), "1.5k");
        assert_eq!(fmt_tokens(118_000), "118k");
        assert_eq!(fmt_tokens(118_499), "118k");
    }

    #[test]
    fn kept_events_still_print_their_lines() {
        let mut state = RenderState::new(false);
        let cases: [(EngineEvent, &str); 7] = [
            (EngineEvent::Steer { text: "focus".into() }, "steer: focus"),
            (EngineEvent::Warning { message: "spend at 80%".into() }, "spend at 80%"),
            (EngineEvent::Error { message: "cracked".into() }, "cracked"),
            (EngineEvent::HeatTick { id: "i1".into(), heat: 2 }, "[i1] heat 2"),
            (EngineEvent::DuelRound { id: "i1".into(), round: 1 }, "[i1] duel round 1"),
            (
                EngineEvent::DuelVerdict { id: "i1".into(), winner: 'a', margin: 3 },
                "[i1] cast a wins by 3",
            ),
            // Heartbeats print their own line — an unattended forge waiting
            // out a rate limit must not look frozen in the logs.
            (
                EngineEvent::ApiRetry { attempt: 2, status: 429, remaining_secs: 240 },
                "api retry 2 · 429 · 240s left",
            ),
        ];
        for (event, needle) in cases {
            let lines = printed(&state.feed(&event));
            assert!(lines.iter().any(|l| l.contains(needle)), "{event:?} → {lines:?}");
        }
    }

    #[test]
    fn renderer_handles_every_variant_without_panicking() {
        for tty in [false, true] {
            let mut state = RenderState::new(tty);
            for event in [
                EngineEvent::TurnStart { turn: 3 },
                EngineEvent::ModelCall { model: "m".into() },
                EngineEvent::ModelRouted { requested: "auto".into(), routed: "r".into() },
                start("bash", "cargo test"),
                result("bash", true, "ok"),
                result("bash", false, "boom"),
                EngineEvent::Tokens {
                    usage: Usage { total_tokens: 42, cost: Some(0.01), ..Default::default() },
                },
                EngineEvent::Steer { text: "focus on tests".into() },
                EngineEvent::Narrate { text: "thinking".into() },
                EngineEvent::Finish { summary: "forged".into() },
                EngineEvent::Error { message: "cracked".into() },
                EngineEvent::Warning { message: "hot".into() },
                EngineEvent::IngotStart { id: "i1".into(), work: "w".into() },
                EngineEvent::HeatTick { id: "i1".into(), heat: 1 },
                EngineEvent::IngotDone { id: "i1".into(), ok: true },
                EngineEvent::DuelRound { id: "i1".into(), round: 1 },
                EngineEvent::DuelVerdict { id: "i1".into(), winner: 'b', margin: 2 },
                EngineEvent::ApiRetry { attempt: 1, status: 529, remaining_secs: 30 },
            ] {
                let _ = state.feed(&event);
            }
            let _ = state.finish();
        }
    }

    /// The JSONL stream carries heartbeats with their fields, so a log
    /// tail can tell a rate-limit wait from a hang.
    #[test]
    fn api_retry_serializes_with_its_fields() {
        let v = serde_json::to_value(EngineEvent::ApiRetry {
            attempt: 3,
            status: 429,
            remaining_secs: 120,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "event": "api_retry",
                "attempt": 3,
                "status": 429,
                "remaining_secs": 120,
            })
        );
    }

    #[test]
    fn live_line_label_and_elapsed_track_the_oldest_unresolved_call() {
        let mut state = RenderState::new(true);
        state.feed(&start("bash", r#"{"command": "cargo build"}"#));
        // A second call starts while the first is still unresolved: the
        // live line keeps describing the first (oldest) call — the same
        // call whose elapsed timer it shows.
        let ops = state.feed(&start("read_file", r#"{"path": "src/x.rs"}"#));
        let live = ops
            .iter()
            .find_map(|op| match op {
                RenderOp::RewriteLive(s) => Some(s.clone()),
                _ => None,
            })
            .expect("live line");
        assert!(live.contains("Run(cargo)"), "{live}");
        assert!(!live.contains("Read(src/x.rs)"), "{live}");
    }

    #[test]
    fn ingot_footer_clears_the_live_line_instead_of_orphaning_it() {
        let mut state = RenderState::new(true);
        state.feed(&EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        state.feed(&start("bash", r#"{"command": "cargo build"}"#));
        // Live line is up; the footer's ClearLive must precede its Print
        // in the op stream or the spinner row is left above the footer.
        let ops = state.feed(&EngineEvent::IngotDone { id: "i1".into(), ok: true });
        let clear_at = ops.iter().position(|op| matches!(op, RenderOp::ClearLive));
        let print_at = ops
            .iter()
            .position(|op| matches!(op, RenderOp::Print(s) if s.contains("[i1] forged")));
        assert!(clear_at.is_some() && print_at.is_some(), "{ops:?}");
        assert!(clear_at < print_at, "clear must precede the footer: {ops:?}");
    }

    /// Item 81: one self-describing run log — typed meta first, ingot
    /// outcomes in the middle, the assay verdict last.
    #[test]
    fn run_log_is_one_self_describing_jsonl_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = RunLog::create(
            dir.path(),
            "20260818_101500-4242",
            RunEntry::RunMeta {
                run_id: "20260818_101500-4242".into(),
                started: "2026-08-18T10:15:00".into(),
                git_branch: Some("main".into()),
                model: "qwen/qwen3-coder".into(),
                duel: "auto".into(),
                flux_profile: "blueprint+alloy".into(),
                crucible_hash: Some("00ff00ff00ff00ff".into()),
            },
        );
        log.append(&RunEntry::IngotDone { id: "i1".into(), ok: true, heat: 2 });
        log.append(&RunEntry::Note { message: "run budget exhausted".into() });
        log.append(&RunEntry::Assay { total: 3, forged: 2, cracked: 1, ok: false });

        let raw = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["entry"], "run_meta");
        assert_eq!(lines[0]["git_branch"], "main");
        assert_eq!(lines[0]["flux_profile"], "blueprint+alloy");
        assert_eq!(lines[0]["crucible_hash"], "00ff00ff00ff00ff");
        assert_eq!(lines[1]["entry"], "ingot_done");
        assert_eq!(lines[1]["heat"], 2);
        assert_eq!(lines[3]["entry"], "assay");
        assert_eq!(lines[3]["ok"], false);
        assert!(
            log.path().file_name().unwrap().to_str().unwrap().starts_with("run-"),
            "{}",
            log.path().display()
        );
    }

    #[test]
    fn run_log_survives_an_unwritable_path() {
        let log = RunLog::create(
            std::path::Path::new("/dev/null/impossible"),
            "x",
            RunEntry::Note { message: "doomed".into() },
        );
        // Must not panic; later appends stay quiet too.
        log.append(&RunEntry::Assay { total: 0, forged: 0, cracked: 0, ok: true });
    }

    #[test]
    fn pipeline_print_clears_a_foreign_live_line_before_an_ingot_footer() {
        // `print_line` routes pipeline prints through the shared live
        // flag: narrator A's parked live row is cleared before the ingot
        // footer lands, never orphaned above it.
        let mut out: Vec<u8> = Vec::new();
        let mut live = false;
        write_ops(&[RenderOp::RewriteLive("  ◐ Run(cargo)".into())], &mut out, &mut live);
        assert!(live);
        write_ops(&[RenderOp::Print("  ✅ [i1] forged".into())], &mut out, &mut live);
        assert!(!live);
        let s = String::from_utf8(out).unwrap();
        let footer_at = s.find("[i1] forged").unwrap();
        let clear_at = s.rfind("\r\x1b[K").unwrap();
        assert!(clear_at < footer_at, "footer must clear the live row first: {s:?}");
    }
}
