//! dashboard — full-screen Ratatui forge view.
//!
//! One `EngineEvent` stream drives three panes: the crucible (left, ingot
//! list with heat-colored status), a rolling event feed (right, narrator
//! -style one-liners), and a bottom bar with token totals, a steer input
//! line, and key hints. Terminal IO stays thin: all state mutation lives
//! in `apply_event` / `handle_key`, which tests drive without a terminal.
//!
//! The dashboard never owns the forge. Detaching (Esc/q) leaves the forge
//! running headless; Ctrl-C sets the shared `CancelFlag` and detaches.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::UnboundedReceiver;

use serde::{Deserialize, Serialize};

use crate::config::CRUCIBLE;
use crate::crucible::CrucibleCounts;
use crate::engine::events::preview;
use crate::engine::{CancelFlag, EngineEvent, SteerQueue, Usage};
use crate::{progress, steer_history, tui};

/// Rolling feed cap.
const FEED_CAP: usize = 200;
/// Draw coalescing: at most one render per frame (~30fps).
const FRAME: Duration = Duration::from_millis(33);
/// How long a first Ctrl-C stays armed. A forge can be twenty minutes of
/// model spend, so one stray keystroke must not throw it away; a second
/// press inside this window is deliberate.
const DOUBLE_PRESS: Duration = Duration::from_millis(800);
/// A forging ingot with no tokens/tool activity for this long tints yellow.
const STALL_WARN: Duration = Duration::from_secs(15);
/// … and red after this long.
const STALL_DEAD: Duration = Duration::from_secs(60);

const HINT: &str = "⏎ steer · ↑ history · ^O expand · q detach · ^C ^C cancel";

/// Once the engine channel closes there is no smith to steer, so the bar
/// stops offering it. The input is not dead, though: it takes the next
/// commission, which is the whole reason the dashboard stays up.
const HINT_DONE: &str = "⏎ new commission · ↑ history · ^O expand · q quit";

/// What the empty input says it is for.
const PLACEHOLDER: &str = "steer the smith";
const PLACEHOLDER_DONE: &str = "describe the next commission";

/// tui.rs palette (crossterm) → ratatui, same values. Runs through the
/// same truecolor downgrade so the dashboard and the stream view never
/// disagree about what "hot" looks like.
fn palette(c: crossterm::style::Color) -> Color {
    match crate::tui::downgrade(c) {
        crossterm::style::Color::Rgb { r, g, b } => Color::Rgb(r, g, b),
        crossterm::style::Color::DarkGrey => Color::DarkGray,
        crossterm::style::Color::Red => Color::Red,
        crossterm::style::Color::White => Color::White,
        crossterm::style::Color::AnsiValue(n) => Color::Indexed(n),
        _ => Color::Reset,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IngotStatus {
    /// In the plan, not on an anvil yet.
    Queued,
    Forging,
    Forged,
    Cracked,
    Duel(u8),
    Verdict { winner: char, margin: u8 },
}

#[derive(Debug, Clone)]
pub(crate) struct IngotRow {
    pub(crate) id: String,
    pub(crate) work: String,
    pub(crate) heat: u8,
    pub(crate) status: IngotStatus,
    /// Last time the forge showed signs of life for this row (tokens or a
    /// tool result). Drives the stalled tint — display only.
    pub(crate) last_activity: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct FeedLine {
    /// Stable row handle. A later event amends an earlier row (the turn
    /// header gains its route, a tool call gains its result), and an index
    /// would not survive the FEED_CAP pop from the front.
    pub(crate) id: u64,
    pub(crate) color: Color,
    pub(crate) text: String,
    /// Full tool output, shown only while Ctrl-O has the feed expanded.
    /// `None` for everything that is already its whole self on one line.
    pub(crate) detail: Option<String>,
    /// Whether the row may wrap. A summary or a verdict is read in full; a
    /// tool row is a list entry, clipped to one line so the list scans.
    pub(crate) wrap: bool,
}

#[derive(Debug, Default)]
pub(crate) struct DashState {
    pub(crate) ingots: Vec<IngotRow>,
    pub(crate) feed: VecDeque<FeedLine>,
    pub(crate) totals: Usage,
    pub(crate) input: String,
    /// Steers pushed but not yet confirmed delivered, oldest first —
    /// mirrors the live `SteerQueue`. A smith only reads its queue at a
    /// turn boundary, so a steer sent mid-tool-call can sit here for
    /// minutes; a 1.5s flash left the user unsure it ever landed.
    pub(crate) queued: Vec<String>,
    /// When Ctrl-C was last pressed, while that press is still armed.
    /// A second press inside `DOUBLE_PRESS` cancels; otherwise the arm
    /// lapses and the next press starts over.
    pub(crate) pending_cancel: Option<Instant>,
    /// Current turn number; picks the metallurgical verb.
    pub(crate) turn: usize,
    /// When the current turn started — the live status line's clock.
    /// `None` between turns and after Finish/Error.
    pub(crate) turn_started: Option<Instant>,
    /// Tokens folded since the turn started (EngineEvent::Tokens deltas).
    pub(crate) turn_tokens: u64,
    /// Ctrl-O: show full tool previews instead of collapsed one-liners.
    pub(crate) expanded: bool,
    /// Latest context fill, as a percentage of the compaction trigger.
    /// `None` before the first turn boundary — the gauge stays off the bar
    /// rather than claiming a confident 0%.
    pub(crate) ctx: Option<(u8, u64, usize)>,
    /// Past steers for the Up arrow, newest first and already deduped —
    /// what `steer_history::recall` hands back at attach.
    pub(crate) history: Vec<String>,
    /// How far Up has walked. `None` means the input is the operator's own
    /// draft, not a recalled entry.
    pub(crate) history_pos: Option<usize>,
    /// What was in the input when the walk started, restored when Down
    /// steps back past the newest entry. Losing a half-typed line to a
    /// stray Up is the failure this field exists to prevent.
    pub(crate) draft: String,
    /// Next row handle. Monotonic, never reused.
    pub(crate) next_row: u64,
    /// The engine channel has closed: no smith is reading the steer queue
    /// any more. Enter stops queueing steers nobody drains and starts
    /// taking the next commission instead.
    pub(crate) finished: bool,
    /// The ingot being forged, named in each turn header. Turn numbers
    /// restart per ingot, so a bare `turn 1` landing under `turn 5` reads
    /// as a broken counter; the id is what makes the reset legible.
    pub(crate) ingot: Option<String>,
    /// The row holding the current turn header, so the route folds into it
    /// rather than spending a line of its own.
    pub(crate) turn_row: Option<u64>,
    /// The tape's clock during a replay; `None` reads the wall clock. Every
    /// elapsed figure on screen (turn timer, stall tint) goes through
    /// `now`, so a replayed frame is the same frame every time.
    pub(crate) clock: Option<Instant>,
    /// The commission, one line, for the header bar.
    pub(crate) commission: Option<String>,
    /// When this view started, for the run clock in the header.
    pub(crate) started: Option<Instant>,
    /// The warden's last word on the goal: met, not met, or not judged.
    pub(crate) goal: Option<bool>,
    /// The route last announced. An unchanged route is not news, so it is
    /// printed once and then only when it moves.
    pub(crate) route: Option<String>,
    /// In-flight tool calls, oldest first per tool name: the row each
    /// result completes in place. Parallel calls of one tool pair FIFO,
    /// which is the order the engine dispatches and collects them in.
    pub(crate) pending_tools: HashMap<String, VecDeque<u64>>,
}

impl DashState {
    pub(crate) fn now(&self) -> Instant {
        self.clock.unwrap_or_else(Instant::now)
    }

    /// Fill the crucible from PLAN.md's ingots, with the status each one
    /// already carries. Events still drive every later change; this only
    /// decides what the pane shows before the first one arrives.
    pub(crate) fn seed_from_plan(&mut self, plan: &str) {
        for ingot in crate::crucible::parse_ingot_lines(plan) {
            let status = match ingot.status {
                crate::sexp::Status::Forged => IngotStatus::Forged,
                crate::sexp::Status::Cracked => IngotStatus::Cracked,
                // Molten means a previous run died mid-ingot. It is not
                // forging now -- nothing is -- so it reads as pending
                // until an event says otherwise.
                crate::sexp::Status::Ore | crate::sexp::Status::Molten => IngotStatus::Queued,
            };
            let row = self.row_mut(&ingot.id);
            row.work = preview(&ingot.work, 60);
            row.heat = ingot.heat;
            row.status = status;
        }
    }

    /// Take in a newer PLAN.md mid-run: rows for ingots the view has not
    /// seen yet (a freshly cast mold, an addendum, a re-smelt's split), and
    /// work text for rows that only had an id. A row an event already
    /// drives keeps its status: events are fresher than a file.
    pub(crate) fn absorb_plan(&mut self, plan: &str) {
        for ingot in crate::crucible::parse_ingot_lines(plan) {
            if let Some(row) = self.ingots.iter_mut().find(|r| r.id == ingot.id) {
                if row.work.is_empty() {
                    row.work = preview(&ingot.work, 60);
                }
                continue;
            }
            let status = match ingot.status {
                crate::sexp::Status::Forged => IngotStatus::Forged,
                crate::sexp::Status::Cracked => IngotStatus::Cracked,
                crate::sexp::Status::Ore | crate::sexp::Status::Molten => IngotStatus::Queued,
            };
            let row = self.row_mut(&ingot.id);
            row.work = preview(&ingot.work, 60);
            row.heat = ingot.heat;
            row.status = status;
        }
    }

    fn row_mut(&mut self, id: &str) -> &mut IngotRow {
        if let Some(i) = self.ingots.iter().position(|r| r.id == id) {
            return &mut self.ingots[i];
        }
        let now = self.now();
        self.ingots.push(IngotRow {
            id: id.to_string(),
            work: String::new(),
            heat: 0,
            status: IngotStatus::Forging,
            last_activity: now,
        });
        self.ingots.last_mut().unwrap()
    }

    /// Refresh the activity clock on every row still forging. Token and
    /// tool events carry no ingot id, so liveness is per-forge, not per-row.
    fn mark_activity(&mut self) {
        let now = self.now();
        for row in &mut self.ingots {
            if row.status == IngotStatus::Forging {
                row.last_activity = now;
            }
        }
    }

    fn push_feed(&mut self, color: Color, text: String) {
        self.push_feed_detail(color, text, None);
    }

    fn push_feed_detail(&mut self, color: Color, text: String, detail: Option<String>) -> u64 {
        let id = self.next_row;
        self.next_row += 1;
        let wrap = !text.starts_with(['→', '✓', '▣']) && !is_tool_failure(&text);
        self.feed.push_back(FeedLine { id, color, text, detail, wrap });
        while self.feed.len() > FEED_CAP {
            self.feed.pop_front();
        }
        id
    }

    /// Rewrite a row that is still on screen. Searches from the back
    /// because every amendment targets a recent row, and returns false
    /// when the row has already aged out past FEED_CAP -- an amendment
    /// for a scrolled-off row is a no-op, never a panic.
    fn amend(&mut self, id: u64, f: impl FnOnce(&mut FeedLine)) -> bool {
        match self.feed.iter_mut().rev().find(|r| r.id == id) {
            Some(row) => {
                f(row);
                true
            }
            None => false,
        }
    }
}

/// The one ingot on an anvil, when there is exactly one: the only case in
/// which a turn or a token can be pinned to an ingot with certainty. The
/// last ingot *started* is not it: in a parallel batch it outlives its
/// siblings' turns and would take credit for them.
fn sole_forging(state: &DashState) -> Option<&str> {
    let mut forging = state
        .ingots
        .iter()
        .filter(|r| matches!(r.status, IngotStatus::Forging | IngotStatus::Duel(_)));
    let first = forging.next()?;
    forging.next().is_none().then_some(first.id.as_str())
}

fn forging_count(state: &DashState) -> usize {
    state
        .ingots
        .iter()
        .filter(|r| matches!(r.status, IngotStatus::Forging | IngotStatus::Duel(_)))
        .count()
}

/// Cut a row to `max` characters without touching its spacing: the verb
/// column is padded, and `preview` would collapse the padding.
fn clip_cells(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// A failed tool row (`✗ verb  subject — error`) as opposed to a failed
/// ingot or an error message: it carries the verb column.
fn is_tool_failure(text: &str) -> bool {
    text.starts_with("✗ ") && text.contains(" — ") && !text.contains(" cracked")
}

/// Fold one engine event into the dashboard state. Pure state mutation —
/// no terminal involved, so tests drive it directly.
pub(crate) fn apply_event(state: &mut DashState, event: EngineEvent) {
    match &event {
        // Spend lives in the header and the turn's status line; a row per
        // model call would push the work itself off the pane.
        EngineEvent::Tokens { usage } => {
            state.totals.add(usage);
            state.turn_tokens += usage.total_tokens;
            state.mark_activity();
            return;
        }
        EngineEvent::TurnStart { turn } => {
            state.turn = *turn;
            state.turn_started = Some(state.now());
            state.turn_tokens = 0;
            // With several anvils hot, turns from different smiths
            // interleave and no event says whose turn this is; a header
            // naming one ingot would be a guess, and a bare number noise.
            if forging_count(state) > 1 {
                state.turn_row = None;
                return;
            }
            let head = match sole_forging(state) {
                Some(ingot) => format!("⚒ {ingot} · turn {turn}"),
                None => format!("⚒ turn {turn}"),
            };
            let id = state.push_feed_detail(palette(tui::COLD), head, None);
            state.turn_row = Some(id);
            return;
        }
        // A route folds into the open turn header, and only when it is
        // news: the first one, or a switch. Three rows become one.
        EngineEvent::ModelRouted { routed, .. } => {
            let fresh = state.route.as_deref() != Some(routed.as_str());
            if fresh {
                state.route = Some(routed.clone());
                if let Some(id) = state.turn_row {
                    let routed = routed.clone();
                    state.amend(id, |row| row.text.push_str(&format!(" · {routed}")));
                }
            }
            return;
        }
        // A call opens a row; its result completes that same row.
        EngineEvent::ToolCallStart { name, preview: p } => {
            state.mark_activity();
            let (verb, arg) = crate::engine::events::tool_label(name, p);
            let id = state.push_feed_detail(
                palette(tui::BRIGHT),
                format!("→ {verb:<6}  {}", preview(arg.as_deref().unwrap_or(""), 120)),
                None,
            );
            state.pending_tools.entry(name.clone()).or_default().push_back(id);
            return;
        }
        EngineEvent::Finish { .. } | EngineEvent::Error { .. } => {
            state.turn_started = None;
        }
        EngineEvent::ToolResult { name, ok, preview: p, lines, bytes, ms } => {
            state.mark_activity();
            let (color, text) = feed_entry(&event);
            let detail = (!p.is_empty()).then(|| p.clone());
            // Pair oldest-first: the engine dispatches a segment of calls
            // then collects them in the same order.
            let pending = state.pending_tools.get_mut(name).and_then(|q| q.pop_front());
            // `finish` adds nothing a row could say: its summary follows.
            if name == "finish" && *ok {
                if let Some(id) = pending {
                    state.feed.retain(|r| r.id != id);
                }
                return;
            }
            let completed = match pending {
                // The call row carries the argument, which the result
                // event does not; keep it and swap the verdict in.
                Some(id) => {
                    let (ok, lines, bytes, ms) = (*ok, *lines, *bytes, *ms);
                    let p = p.clone();
                    state.amend(id, |row| {
                        let arg = row.text.split_once("  ").map(|(_, a)| a.trim().to_string());
                        row.color = color;
                        row.text = complete_row(name, arg.as_deref(), ok, &p, lines, bytes, ms);
                        row.detail = detail.clone();
                    })
                }
                None => false,
            };
            // An orphan result (no call row on screen, or it aged out)
            // still deserves its line rather than vanishing.
            if !completed {
                state.push_feed_detail(color, text, detail);
            }
            return;
        }
        EngineEvent::Warning { message: text } | EngineEvent::Narrate { text }
            if crate::render::echo::warden_word(text).is_some() =>
        {
            state.goal = crate::render::echo::warden_word(text).map(|w| w.starts_with("goal met"));
        }
        EngineEvent::ContextGauge { pct, used_tokens, budget_tokens } => {
            state.ctx = Some((*pct, *used_tokens, *budget_tokens));
        }
        // Delivery confirmed: drop the oldest matching entry. Matching by
        // text, not by index, because the engine may drain several at
        // once and duplicates of the same steer are legitimate.
        EngineEvent::Steer { text } => {
            if let Some(i) = state.queued.iter().position(|q| q == text) {
                state.queued.remove(i);
            }
        }
        EngineEvent::IngotStart { id, work } => {
            state.ingot = Some(id.clone());
            state.route = None;
            let now = state.now();
            let row = state.row_mut(id);
            row.work = preview(work, 60);
            row.heat = 0;
            row.status = IngotStatus::Forging;
            row.last_activity = now;
        }
        EngineEvent::HeatTick { id, heat } => state.row_mut(id).heat = *heat,
        EngineEvent::IngotDone { id, ok } => {
            state.row_mut(id).status =
                if *ok { IngotStatus::Forged } else { IngotStatus::Cracked };
        }
        EngineEvent::DuelRound { id, round } => {
            state.row_mut(id).status = IngotStatus::Duel(*round);
        }
        EngineEvent::DuelVerdict { id, winner, margin } => {
            state.row_mut(id).status =
                IngotStatus::Verdict { winner: *winner, margin: *margin };
        }
        _ => {}
    }
    let (color, text) = feed_entry(&event);
    // An empty entry is a deliberate silence (the gauge renders in the
    // bottom bar instead); pushing it would burn a feed slot on nothing.
    if text.is_empty() {
        return;
    }
    // Only tool results have more to show than their one-liner.
    let detail = match &event {
        EngineEvent::ToolResult { preview: p, .. } if !p.is_empty() => Some(p.clone()),
        _ => None,
    };
    state.push_feed_detail(color, text, detail);
}

/// The completed form of a tool row: the call's argument kept, the verdict
/// swapped in. A success is quiet and says only what it is hiding; a
/// failure shows its error inline, the one result worth reading unexpanded.
fn complete_row(
    name: &str,
    arg: Option<&str>,
    ok: bool,
    err: &str,
    lines: usize,
    _bytes: usize,
    ms: u64,
) -> String {
    let (verb, _) = crate::engine::events::tool_label(name, "");
    let arg = match arg {
        Some(a) if !a.is_empty() => format!("  {a}"),
        _ => String::new(),
    };
    if ok {
        let mut meta = Vec::new();
        if lines > 1 {
            meta.push(format!("{lines} lines"));
        }
        if ms >= 1000 {
            meta.push(format!("{:.1}s", ms as f64 / 1000.0));
        }
        let meta = if meta.is_empty() { String::new() } else { format!("  · {}", meta.join(" · ")) };
        format!("✓ {verb:<6}{arg}{meta}")
    } else {
        let gist = crate::engine::events::tool_error(name, err);
        format!("✗ {verb:<6}{arg} — {}", preview(&gist, 100))
    }
}

/// One narrator-style feed line per event, in the stream view's words.
/// An empty text is a deliberate silence.
fn feed_entry(event: &EngineEvent) -> (Color, String) {
    match event {
        // The gauge already lives in the bottom bar; a feed line per turn
        // would say the same thing twice and push real events off screen.
        EngineEvent::ContextGauge { .. } => (palette(tui::COLD), String::new()),
        EngineEvent::TurnStart { turn } => (palette(tui::COLD), format!("⚒ turn {turn}")),
        // The model and the route it resolved to fold into the turn
        // header (see `apply_event`). Printed as their own rows they
        // repeated verbatim every turn and cost a quarter of the feed.
        EngineEvent::ModelCall { .. } | EngineEvent::ModelRouted { .. } => {
            (palette(tui::COLD), String::new())
        }
        EngineEvent::ToolCallStart { name, preview: p } => {
            let (verb, arg) = crate::engine::events::tool_label(name, p);
            (palette(tui::BRIGHT), format!("→ {verb:<6}  {}", preview(arg.as_deref().unwrap_or(""), 120)))
        }
        EngineEvent::ToolResult { name, ok, preview: p, lines, bytes, ms } => {
            let color = if *ok { palette(tui::COLD) } else { palette(tui::WARM) };
            (color, complete_row(name, None, *ok, p, *lines, *bytes, *ms))
        }
        EngineEvent::Tokens { .. } => (palette(tui::COLD), String::new()),
        EngineEvent::Steer { text } => {
            (palette(tui::BRIGHT), format!("↪ steer  {}", preview(text, 200)))
        }
        EngineEvent::Finish { summary } => match crate::engine::events::verdict_gist(summary) {
            Some((true, text)) => (palette(tui::PURE), format!("⚖ judged pass — {text}")),
            Some((false, text)) => (palette(tui::WARM), format!("⚖ judged fail — {text}")),
            None => (
                palette(tui::PURE),
                format!("■ {}", crate::engine::events::finish_gist(summary).unwrap_or_else(|| preview(summary, 400))),
            ),
        },
        EngineEvent::Error { message } => (palette(tui::WARM), format!("✗ {}", preview(message, 200))),
        EngineEvent::Narrate { text } => match crate::render::echo::warden_word(text) {
            Some(word) => warden_row(text, word),
            None => (palette(tui::COLD), format!("◈ {}", preview(text, 200))),
        },
        EngineEvent::Warning { message } => warning_row(message),
        EngineEvent::IngotStart { id, work } => (
            palette(tui::HOT),
            format!("▣ {id} · {}", preview(&crate::render::echo::title(work), 120)),
        ),
        // The first heat is the ingot's start; only a re-heat is news.
        EngineEvent::HeatTick { id, heat } if *heat >= 2 => {
            (palette(tui::BRIGHT), format!("↻ {id} heat {heat}"))
        }
        EngineEvent::HeatTick { .. } => (palette(tui::COLD), String::new()),
        EngineEvent::IngotDone { id, ok: true } => (palette(tui::PURE), format!("✓ {id} forged")),
        EngineEvent::IngotDone { id, ok: false } => (palette(tui::WARM), format!("✗ {id} cracked")),
        EngineEvent::DuelRound { id, round } => {
            (palette(tui::BRIGHT), format!("⚔ {id} duel round {round}"))
        }
        EngineEvent::DuelVerdict { id, winner, margin } => {
            (palette(tui::PURE), format!("⚖ {id} cast {winner} wins by {margin}"))
        }
        EngineEvent::ApiRetry { attempt, status, remaining_secs } => (
            palette(tui::BRIGHT),
            format!("⟳ api {status} · retry {attempt} · {remaining_secs}s left"),
        ),
        EngineEvent::HookStarted { name, hook_event, status_message } => (
            palette(tui::COLD),
            status_message
                .clone()
                .unwrap_or_else(|| format!("⚓ {hook_event} hook {name}")),
        ),
        EngineEvent::HookFinished { name, code, duration_ms, .. } => {
            let verdict = match code {
                0 => "ok",
                2 => "blocked",
                _ => "failed",
            };
            (palette(tui::COLD), format!("⚓ hook {name} {verdict} ({duration_ms}ms)"))
        }
    }
}

/// The warden's word on the goal, as one feed row.
fn warden_row(message: &str, word: &str) -> (Color, String) {
    let round = message
        .strip_prefix("warden round ")
        .and_then(|r| r.split(':').next())
        .unwrap_or("?");
    if word.starts_with("goal met") {
        (palette(tui::PURE), format!("✓ warden · round {round} · {word}"))
    } else {
        (palette(tui::WARM), format!("✗ warden · round {round} · goal not met — {word}"))
    }
}

/// A warning, read for what it is (see `engine::events::warning_lines`).
fn warning_row(message: &str) -> (Color, String) {
    if let Some(word) = crate::render::echo::warden_word(message) {
        return warden_row(message, word);
    }
    if crate::render::tape::rewound(message).is_some() {
        // Housekeeping after every failed heat; the ↻ row already said it.
        return (palette(tui::COLD), String::new());
    }
    if let Some(rest) = message.strip_prefix("warden rejected the ") {
        let (doc, gap) = rest.split_once(": ").unwrap_or((rest, ""));
        return (
            palette(tui::BRIGHT),
            format!("! warden sent back the {} — {}", doc.to_lowercase(), preview(gap, 200)),
        );
    }
    (palette(tui::BRIGHT), format!("! {}", preview(message, 200)))
}

/// The bottom bar's live status while a turn runs:
/// `⚒ Forging… i2 · 12s · 4.1k tok · 342 tok/s`. `Some` only while a turn
/// is running and at least one ingot is still forging; otherwise the bar
/// says where the run stands instead.
pub(crate) fn forge_status(state: &DashState, now: Instant) -> Option<String> {
    let started = state.turn_started?;
    if !state.ingots.iter().any(|r| r.status == IngotStatus::Forging) {
        return None;
    }
    let elapsed = now.saturating_duration_since(started);
    let mut s = format!("⚒ {}…", tui::forge_verb(state.turn));
    match (forging_count(state), sole_forging(state)) {
        (_, Some(ingot)) => s.push_str(&format!(" {ingot} ·")),
        (n, None) if n > 1 => s.push_str(&format!(" {n} anvils ·")),
        _ => {}
    }
    s.push_str(&format!(" {}s · {} tok", elapsed.as_secs(), tui::tokens(state.turn_tokens)));
    if let Some(rate) = crate::progress::token_rate(state.turn_tokens, elapsed) {
        s.push_str(&format!(" · {rate} tok/s"));
    }
    Some(s)
}

/// True when any forging row has crossed the stall threshold.
pub(crate) fn has_stalled(state: &DashState, now: Instant) -> bool {
    state.ingots.iter().any(|row| {
        row.status == IngotStatus::Forging
            && now.saturating_duration_since(row.last_activity) >= STALL_WARN
    })
}

/// Ingot lifecycle events are the only ones that move the terminal
/// chrome (title + taskbar progress).
fn is_ingot_event(ev: &EngineEvent) -> bool {
    matches!(ev, EngineEvent::IngotStart { .. } | EngineEvent::IngotDone { .. })
}

fn is_crack(ev: &EngineEvent) -> bool {
    matches!(ev, EngineEvent::IngotDone { ok: false, .. })
}

/// Dashboard rows folded back into crucible-shaped counts, plus the id of
/// the most recently started ingot still forging (for the title). Total is
/// only the rows this view has seen — `run` bumps it to the plan's true
/// size so the title ratio never shrinks its denominator mid-run.
pub(crate) fn dash_counts(state: &DashState) -> (CrucibleCounts, Option<String>) {
    let mut counts = CrucibleCounts { total: state.ingots.len(), ..Default::default() };
    let mut active = None;
    for row in &state.ingots {
        match row.status {
            IngotStatus::Queued => counts.ore += 1,
            IngotStatus::Forged => counts.forged += 1,
            IngotStatus::Cracked => counts.cracked += 1,
            IngotStatus::Forging | IngotStatus::Duel(_) | IngotStatus::Verdict { .. } => {
                counts.molten += 1;
                if row.status == IngotStatus::Forging {
                    active = Some(row.id.clone());
                }
            }
        }
    }
    (counts, active)
}

// ------------------------------------------------------- session cost state
//
// Each invocation's Usage totals start at zero, so a resumed job used to
// report only the last invocation's spend. The record in
// `.slag/session-costs.json` (keyed by run id) carries whole-job spend
// across resumes: `run` seeds its totals from it, and saves the cumulative
// figure back on exit and on every crack.

/// Where whole-job spend survives between invocations.
pub const SESSION_COSTS: &str = ".slag/session-costs.json";

/// One run's cumulative spend, JSON-shaped for `.slag/session-costs.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CostRecord {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cost: Option<f64>,
}

impl CostRecord {
    pub fn from_usage(u: &Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cost: u.cost,
        }
    }

    pub fn to_usage(&self) -> Usage {
        Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cost: self.cost,
            ..Default::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.total_tokens == 0 && self.cost.is_none()
    }
}

/// FNV-1a — deterministic across runs and toolchains, unlike
/// `DefaultHasher` (whose algorithm is not a stability guarantee).
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Stable run id for a plan: the crucible's `;; CRUCIBLE <timestamp>`
/// header, hashed. The header survives every save — including re-smelt
/// rewrites and splits, which change the ingot set — so one job keeps one
/// id across resumes, and a freshly cast plan starts a fresh record.
pub fn run_id_for_plan(plan: &str) -> String {
    plan.lines()
        .map(str::trim_start)
        .find(|l| l.starts_with(";; CRUCIBLE"))
        .map(|l| format!("{:016x}", fnv1a(l.trim_end())))
        .unwrap_or_else(|| "default".into())
}

/// Run id for the crucible on disk RIGHT NOW. The dashboard spawns
/// before surveyor/founder create PLAN.md on a fresh commission, so the
/// id must be recomputed at every save — an id captured at start would
/// be "default" and the whole first invocation's spend (surveyor +
/// founder included) would be lost to resumes.
fn current_run_id() -> String {
    run_id_for_plan(&std::fs::read_to_string(CRUCIBLE).unwrap_or_default())
}

/// "default" (no `;; CRUCIBLE` header on disk) is not a job identity:
/// spend saved under it would leak into every later job in the same
/// directory, and seeding from it would display unrelated jobs' spend.
fn persistable(run_id: &str) -> bool {
    run_id != "default"
}

fn load_session_costs_at(path: &Path) -> BTreeMap<String, CostRecord> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Read one run's persisted spend from an explicit file (tests inject a
/// scratch path; production goes through `load_session_cost`).
pub fn load_session_cost_at(path: &Path, run_id: &str) -> Option<CostRecord> {
    load_session_costs_at(path).remove(run_id)
}

/// Merge one run's record into the costs file, keeping other runs' rows.
/// Replace, not add: the caller's record is already cumulative (its totals
/// were seeded from this file), so adding would double-count.
pub fn save_session_cost_at(
    path: &Path,
    run_id: &str,
    record: &CostRecord,
) -> io::Result<()> {
    let mut map = load_session_costs_at(path);
    map.insert(run_id.to_string(), record.clone());
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(path, serde_json::to_string_pretty(&map)?)
}

pub fn load_session_cost(run_id: &str) -> Option<CostRecord> {
    load_session_cost_at(Path::new(SESSION_COSTS), run_id)
}

/// Best-effort persist: a failed write must never take the dashboard down.
fn save_session_cost(run_id: &str, record: &CostRecord) {
    if record.is_empty() || !persistable(run_id) {
        return;
    }
    let _ = save_session_cost_at(Path::new(SESSION_COSTS), run_id, record);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    Stay,
    /// Leave the dashboard; the forge continues headless.
    Detach,
    /// CancelFlag set; leave the dashboard.
    Cancel,
    /// The run is over and the operator typed a new commission. The
    /// dashboard hands it back so the caller can forge again instead of
    /// making a finished screen a full stop.
    Commission(String),
}

/// Fold one key press into the state. Pure except for the steer queue
/// push and the cancel-flag store.
pub(crate) fn handle_key(
    state: &mut DashState,
    key: KeyEvent,
    steer: &SteerQueue,
    cancel: &CancelFlag,
) -> KeyOutcome {
    handle_key_at(state, key, steer, cancel, Instant::now())
}

/// Clock-injected form. The double-press window is the only time-dependent
/// branch here, and asserting on it by sleeping 800ms in a test is both
/// slow and flaky.
pub(crate) fn handle_key_at(
    state: &mut DashState,
    key: KeyEvent,
    steer: &SteerQueue,
    cancel: &CancelFlag,
    now: Instant,
) -> KeyOutcome {
    if key.kind != KeyEventKind::Press {
        return KeyOutcome::Stay;
    }
    // Feed the notification idleness gate: a user actively typing here
    // must not get a desktop ping for the finish they are watching.
    tui::mark_user_activity();
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        // Armed and still inside the window: they meant it.
        if state
            .pending_cancel
            .is_some_and(|t| now.saturating_duration_since(t) < DOUBLE_PRESS)
        {
            state.pending_cancel = None;
            cancel.store(true, Ordering::SeqCst);
            return KeyOutcome::Cancel;
        }
        // First press (or a lapsed one): arm and say so. The bottom bar
        // reads `pending_cancel` to flash "press again to cancel".
        state.pending_cancel = Some(now);
        return KeyOutcome::Stay;
    }
    // Ctrl-O swaps collapsed one-liners for full previews. One toggle,
    // one hint in the HINT bar — not a per-line "ctrl-o to expand" that
    // would repeat itself two hundred times down the feed.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'))
    {
        state.pending_cancel = None;
        state.expanded = !state.expanded;
        return KeyOutcome::Stay;
    }
    // Any other key is evidence the Ctrl-C was a slip; disarm so a later
    // stray press cannot pair with it across minutes of typing.
    state.pending_cancel = None;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') if state.input.is_empty() => KeyOutcome::Detach,
        KeyCode::Esc => {
            state.input.clear();
            state.history_pos = None;
            state.draft.clear();
            KeyOutcome::Stay
        }
        KeyCode::Enter => {
            if !state.input.is_empty() {
                let text = std::mem::take(&mut state.input);
                // `ctx` is a local query, not a steer: it prints the
                // breakdown behind the gauge instead of costing a turn.
                if text.trim() == "ctx" {
                    state.push_feed(palette(tui::COLD), ctx_breakdown(state.ctx));
                    return KeyOutcome::Stay;
                }
                // No smith, no steer. Before this, Enter pushed into a
                // queue with no reader and the keystroke vanished.
                if state.finished {
                    steer_history::record(&text);
                    state.history.retain(|h| h != &text);
                    state.history.insert(0, text.clone());
                    state.history_pos = None;
                    state.draft.clear();
                    return KeyOutcome::Commission(text);
                }
                if let Ok(mut q) = steer.lock() {
                    q.push(text.clone());
                }
                // Buffered, not written: the disk is off the keypress path.
                // The shutdown registry lands it.
                steer_history::record(&text);
                // The submitted steer becomes the newest recall entry, and
                // any older copy of it goes — pressing Up four times should
                // reach four different steers, not one repeated.
                state.history.retain(|h| h != &text);
                state.history.insert(0, text.clone());
                state.history_pos = None;
                state.draft.clear();
                state.queued.push(text);
            }
            KeyOutcome::Stay
        }
        // Up/Down walk past steers. Only meaningful with history to walk,
        // so an empty list leaves the arrows inert rather than clearing
        // the line under the operator.
        KeyCode::Up if !state.history.is_empty() => {
            let next = match state.history_pos {
                // Starting the walk: stash whatever was typed so Down can
                // put it back.
                None => {
                    state.draft = state.input.clone();
                    0
                }
                // At the oldest already: hold there.
                Some(i) => (i + 1).min(state.history.len() - 1),
            };
            state.history_pos = Some(next);
            state.input = state.history[next].clone();
            KeyOutcome::Stay
        }
        KeyCode::Down if state.history_pos.is_some() => {
            match state.history_pos {
                Some(0) | None => {
                    // Back past the newest: the operator's own line returns.
                    state.history_pos = None;
                    state.input = std::mem::take(&mut state.draft);
                }
                Some(i) => {
                    state.history_pos = Some(i - 1);
                    state.input = state.history[i - 1].clone();
                }
            }
            KeyOutcome::Stay
        }
        KeyCode::Backspace => {
            // Editing a recalled steer makes it the draft: the walk is over,
            // and a later Down must not discard the edit.
            state.history_pos = None;
            state.input.pop();
            KeyOutcome::Stay
        }
        KeyCode::Char(c) => {
            state.history_pos = None;
            state.input.push(c);
            KeyOutcome::Stay
        }
        _ => KeyOutcome::Stay,
    }
}

// ---------------------------------------------------------------- rendering

pub(crate) fn draw(f: &mut Frame, state: &DashState) {
    let [top, main, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(bottom_height(state)),
    ])
    .areas(f.area());
    draw_header(f, top, state);
    // The crucible gets the room its titles need and no more; below 60
    // columns it gives way entirely, since a feed squeezed beside it is
    // worth less than a feed alone.
    let w = main.width;
    let crucible = if w < 60 { 0 } else { (w * 38 / 100).clamp(28, 56) };
    let [left, right] =
        Layout::horizontal([Constraint::Length(crucible), Constraint::Min(10)]).areas(main);
    if crucible > 0 {
        draw_crucible(f, left, state);
    }
    draw_feed(f, right, state);
    draw_bottom(f, bottom, state);
}

/// One row on top: the wordmark, the commission, and where the run stands.
/// `░▒▓█ SLAG  wc-lite CLI with tests …        forging i2 · 3:12 · 41k tok · $0.06`
fn draw_header(f: &mut Frame, area: Rect, state: &DashState) {
    let width = area.width as usize;
    let mut spans = vec![Span::raw(" ")];
    for (glyph, c) in [("░", tui::COLD), ("▒", tui::WARM), ("▓", tui::HOT), ("█", tui::BRIGHT)] {
        spans.push(Span::styled(glyph, Style::default().fg(palette(c))));
    }
    spans.push(Span::styled(
        " SLAG ",
        Style::default().fg(palette(tui::PURE)).add_modifier(ratatui::style::Modifier::BOLD),
    ));
    let now = state.now();
    let mut right: Vec<String> = vec![run_phase(state)];
    if let Some(start) = state.started {
        right.push(tui::clock(now.saturating_duration_since(start)));
    }
    if state.totals.total_tokens > 0 {
        right.push(format!("{} tok", tui::tokens(state.totals.total_tokens)));
    }
    if let Some(c) = state.totals.cost {
        right.push(tui::money(c));
    }
    let right = format!("{} ", right.join(" · "));
    let used = 11 + right.chars().count();
    if let Some(c) = &state.commission {
        let room = width.saturating_sub(used + 2);
        if room >= 12 {
            spans.push(Span::styled(
                format!(" {}", preview(c, room)),
                Style::default().fg(palette(tui::COLD)),
            ));
        }
    }
    let left_w: usize = spans.iter().map(|s| s.width()).sum();
    let pad = width.saturating_sub(left_w + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    let failed = state.ingots.iter().any(|r| r.status == IngotStatus::Cracked) || state.goal == Some(false);
    let phase_color = match (state.finished, failed) {
        (true, true) => palette(tui::WARM),
        (true, false) => palette(tui::PURE),
        _ => palette(tui::HOT),
    };
    spans.push(Span::styled(right, Style::default().fg(phase_color)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// `forging i1 i2`, `planning`, `finished`: the run in two words.
fn run_phase(state: &DashState) -> String {
    if state.finished {
        let cracked = state.ingots.iter().filter(|r| r.status == IngotStatus::Cracked).count();
        return match (cracked, state.goal) {
            (n, _) if n > 0 => format!("finished · {n} cracked"),
            (_, Some(false)) => "finished · goal not met".into(),
            (_, Some(true)) => "finished · goal met".into(),
            _ => "finished".into(),
        };
    }
    let forging: Vec<&str> = state
        .ingots
        .iter()
        .filter(|r| matches!(r.status, IngotStatus::Forging | IngotStatus::Duel(_)))
        .map(|r| r.id.as_str())
        .collect();
    if !forging.is_empty() {
        let shown: Vec<&str> = forging.iter().take(3).copied().collect();
        return format!("forging {}", shown.join(" "));
    }
    if state.turn_started.is_some() || state.turn > 0 {
        return "planning".into();
    }
    "waiting".into()
}

/// The crucible's name for an ingot: what it *is*, not what it is doing.
///
/// The glyph and colour already carry the status, so a column of `forged`
/// down the pane repeats what a reader can see and hides the one thing
/// that tells the rows apart. Plans tend to open every task with a `GOAL:`
/// or `TASK:` label, which is boilerplate once it is on every row, so it
/// comes off. Truncation lands on a word boundary: a title cut mid-word
/// reads as corruption rather than as an abbreviation.
fn ingot_title(work: &str, width: usize) -> String {
    let named = crate::render::echo::title(work);
    let t = named.trim();
    if t.chars().count() <= width {
        return t.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    // Reserve one column for the ellipsis, then back off to a word break
    // when one is near enough to be worth losing a word for.
    let budget = width - 1;
    let head: String = t.chars().take(budget).collect();
    let cut = match head.rfind(' ') {
        Some(i) if i >= budget.saturating_sub(14) && i > 0 => i,
        _ => head.len(),
    };
    format!("{}…", head[..cut].trim_end())
}

/// One crucible row: `⚒ i2  counting core with no I/O       2/5`. The
/// glyph carries the status; the right column carries the heat once there
/// has been a retry, or whatever else is news (a stall, a duel).
fn ingot_line(row: &IngotRow, now: Instant, width: usize) -> Line<'_> {
    let (glyph, mut word, mut color) = match &row.status {
        IngotStatus::Queued => ("·", "queued".to_string(), palette(tui::COLD)),
        IngotStatus::Forging => {
            ("⚒", "forging".to_string(), palette(tui::heat_color(row.heat)))
        }
        IngotStatus::Forged => ("✓", "forged".to_string(), palette(tui::PURE)),
        IngotStatus::Cracked => ("✗", "cracked".to_string(), palette(tui::WARM)),
        IngotStatus::Duel(r) => {
            ("⚔", format!("duel r{r}"), palette(tui::heat_color(row.heat)))
        }
        IngotStatus::Verdict { winner, margin } => {
            ("⚖", format!("cast {winner} +{margin}"), palette(tui::BRIGHT))
        }
    };
    let mut stalled = false;
    if row.status == IngotStatus::Forging {
        let silent = now.saturating_duration_since(row.last_activity);
        if silent >= STALL_WARN {
            stalled = true;
            word = format!("stalled {}s", silent.as_secs());
            color = if silent >= STALL_DEAD {
                palette(tui::WARM)
            } else {
                palette(tui::BRIGHT)
            };
        }
    }
    // A status word earns its place only when it says something the glyph
    // does not. `forged` under a ✓ does not; a stall, a duel round, a
    // verdict, or a retry count does.
    let note = match &row.status {
        _ if stalled => Some(word),
        IngotStatus::Duel(_) | IngotStatus::Verdict { .. } => Some(word),
        IngotStatus::Forging | IngotStatus::Forged | IngotStatus::Cracked if row.heat > 1 => {
            Some(format!("heat {}", row.heat))
        }
        _ => None,
    };
    // Fixed columns: glyph, space, id, two spaces, and the note with a
    // space before it. Whatever is left belongs to the title.
    let id_w = row.id.chars().count();
    let note_w = note.as_ref().map_or(0, |n| n.chars().count() + 1);
    let room = width.saturating_sub(2 + id_w + 2 + note_w);
    let title = ingot_title(&row.work, room);
    let id_color = match row.status {
        IngotStatus::Queued => palette(tui::COLD),
        _ => palette(tui::PURE),
    };
    let mut spans = vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(format!("{}  ", row.id), Style::default().fg(id_color)),
    ];
    // An ingot announced but not yet described still needs a name; its
    // status is the only one it has.
    if title.is_empty() {
        spans.push(Span::styled(
            note.clone().unwrap_or_else(|| status_word(&row.status)),
            Style::default().fg(color),
        ));
        return Line::from(spans);
    }
    let title_w = title.chars().count();
    let title_color = match row.status {
        IngotStatus::Queued => palette(tui::COLD),
        IngotStatus::Cracked => palette(tui::WARM),
        _ => palette(tui::PURE),
    };
    spans.push(Span::styled(title, Style::default().fg(title_color)));
    if let Some(n) = note {
        let pad = width.saturating_sub(2 + id_w + 2 + title_w + n.chars().count()).max(1);
        spans.push(Span::styled(format!("{}{n}", " ".repeat(pad)), Style::default().fg(color)));
    }
    Line::from(spans)
}

/// The bare status word, for a row that has no work text to show instead.
fn status_word(status: &IngotStatus) -> String {
    match status {
        IngotStatus::Queued => "queued".to_string(),
        IngotStatus::Forging => "forging".to_string(),
        IngotStatus::Forged => "forged".to_string(),
        IngotStatus::Cracked => "cracked".to_string(),
        IngotStatus::Duel(r) => format!("duel r{r}"),
        IngotStatus::Verdict { winner, margin } => format!("cast {winner} +{margin}"),
    }
}

fn pane(title: Line<'static>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .border_style(Style::default().fg(palette(tui::COLD)))
}

fn draw_crucible(f: &mut Frame, area: Rect, state: &DashState) {
    let inner_w = area.width.saturating_sub(4) as usize;
    let rows_room = area.height.saturating_sub(2) as usize;
    let (counts, _) = dash_counts(state);
    let mut title = vec![
        Span::styled(" crucible ", Style::default().fg(palette(tui::PURE))),
    ];
    if counts.total > 0 {
        title.push(Span::styled(
            format!("{}/{} ", counts.forged, counts.total),
            Style::default().fg(palette(tui::COLD)),
        ));
    }
    if counts.cracked > 0 {
        title.push(Span::styled(
            format!("· {} cracked ", counts.cracked),
            Style::default().fg(palette(tui::WARM)),
        ));
    }
    let mut lines: Vec<Line> = Vec::new();
    if state.ingots.is_empty() {
        let empty = if state.finished { "no ingots were cast" } else { "cast after the survey" };
        lines.push(Line::from(Span::styled(
            preview(empty, inner_w),
            Style::default().fg(palette(tui::COLD)),
        )));
    } else {
        // The bar takes the pane's last row when there is room for it and
        // at least one ingot above it.
        let bar_rows = if rows_room >= 3 { 2 } else { 0 };
        let visible = rows_room.saturating_sub(bar_rows);
        let skip = state.ingots.len().saturating_sub(visible);
        let now = state.now();
        lines.extend(state.ingots.iter().skip(skip).map(|row| ingot_line(row, now, inner_w)));
        if bar_rows > 0 {
            while lines.len() < visible {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(""));
            lines.push(bar_line(&counts, inner_w));
        }
    }
    let block = pane(Line::from(title)).padding(ratatui::widgets::Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The fire bar in ratatui spans: forged cells warm up along the bar,
/// cracked cells end the fill in WARM, the rest stays COLD.
fn bar_line(counts: &CrucibleCounts, width: usize) -> Line<'static> {
    let pct = counts.forged * 100 / counts.total.max(1);
    let label = format!(" {pct}%");
    let cells = width.saturating_sub(label.len()).max(4);
    let total = counts.total.max(1);
    let filled = ((counts.forged * cells + total / 2) / total).min(cells);
    let cracked = ((counts.cracked * cells + total / 2) / total).min(cells - filled);
    let mut spans = Vec::new();
    for i in 0..filled {
        let c = match i * 3 / cells.max(1) {
            0 => tui::WARM,
            1 => tui::HOT,
            _ => tui::BRIGHT,
        };
        spans.push(Span::styled("█", Style::default().fg(palette(c))));
    }
    spans.push(Span::styled("▓".repeat(cracked), Style::default().fg(palette(tui::WARM))));
    spans.push(Span::styled(
        "░".repeat(cells - filled - cracked),
        Style::default().fg(palette(tui::COLD)),
    ));
    spans.push(Span::styled(label, Style::default().fg(palette(tui::PURE))));
    Line::from(spans)
}

/// Break one feed row to the pane width, on char boundaries.
///
/// The feed clipped every row at the border, which cost most on the one
/// row worth reading in full: a finish summary is the turn's conclusion,
/// and it was landing half off-screen. Continuations are indented so a
/// wrapped row still reads as one row.
pub(crate) fn wrap_row(text: &str, width: usize) -> Vec<String> {
    if width < 8 {
        // Too narrow to wrap usefully; the pane is already unusable and a
        // one-char-per-line column would be worse than a clip.
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() { word.chars().count() } else { line.chars().count() + 1 + word.chars().count() };
        if candidate > width && !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        // A single word longer than the pane still has to break somewhere.
        if word.chars().count() > width {
            let mut chunk = String::new();
            for c in word.chars() {
                if chunk.chars().count() == width {
                    out.push(std::mem::take(&mut chunk));
                }
                chunk.push(c);
            }
            line = chunk;
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// A summary row wraps to at most this many lines while the feed is
/// collapsed; Ctrl-O shows it whole.
const SUMMARY_LINES: usize = 4;

fn draw_feed(f: &mut Frame, area: Rect, state: &DashState) {
    // Render first, then scroll: a wrapped row and an expanded detail both
    // spend more than one line, so counting feed rows would scroll past
    // the newest output exactly when there is most of it to read.
    let inner = area.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for l in state.feed.iter() {
        let mut chunks = if l.wrap || state.expanded {
            wrap_row(&l.text, inner.saturating_sub(2))
        } else {
            vec![clip_cells(&l.text, inner)]
        };
        if !state.expanded && chunks.len() > SUMMARY_LINES {
            chunks.truncate(SUMMARY_LINES);
            if let Some(last) = chunks.last_mut() {
                *last = preview(&format!("{last} …"), inner.saturating_sub(4));
            }
        }
        for (i, chunk) in chunks.into_iter().enumerate() {
            let indent = if i == 0 { "" } else { "  " };
            lines.push(Line::from(Span::styled(
                format!("{indent}{chunk}"),
                Style::default().fg(l.color),
            )));
        }
        if !state.expanded {
            continue;
        }
        if let Some(detail) = &l.detail {
            lines.extend(detail_lines(detail));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "waiting for the first turn",
            Style::default().fg(palette(tui::COLD)),
        )));
    }
    let visible = area.height.saturating_sub(2) as usize;
    if lines.len() > visible {
        lines.drain(..lines.len() - visible);
    }
    let title = if state.expanded { " feed · expanded " } else { " feed " };
    let block = pane(Line::from(Span::styled(title, Style::default().fg(palette(tui::PURE)))))
        .padding(ratatui::widgets::Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Expanded tool output, indented under its one-liner. A preview that
/// already reads as a unified diff (what `edit_file` returns) is
/// re-rendered through `render::diff` so the word that actually changed
/// is highlighted instead of the whole line going red-then-green.
pub(crate) fn detail_lines(detail: &str) -> Vec<Line<'static>> {
    if let Some((old, new)) = diff_sides(detail) {
        return crate::render::diff::diff_lines(&old, &new)
            .into_iter()
            .map(diff_line_to_ratatui)
            .collect();
    }
    detail
        .lines()
        .map(|t| {
            Line::from(Span::styled(
                format!("      {t}"),
                Style::default().fg(palette(tui::COLD)),
            ))
        })
        .collect()
}

/// Recover the before/after sides of a unified-diff-shaped preview. Any
/// line the tool did not mark is context and belongs to both sides.
/// `None` when nothing is marked — a plain output is not a diff.
fn diff_sides(detail: &str) -> Option<(String, String)> {
    let (mut old, mut new) = (String::new(), String::new());
    let mut marked = false;
    for line in detail.lines() {
        // `---`/`+++` are the file headers, not content.
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'-') => {
                marked = true;
                old.push_str(&line[1..]);
                old.push('\n');
            }
            Some(b'+') => {
                marked = true;
                new.push_str(&line[1..]);
                new.push('\n');
            }
            _ => {
                let body = line.strip_prefix(' ').unwrap_or(line);
                old.push_str(body);
                old.push('\n');
                new.push_str(body);
                new.push('\n');
            }
        }
    }
    marked.then_some((old, new))
}

/// A `DiffLine` in slag's palette: removals WARM, additions HOT, context
/// COLD, and the spans that actually moved rendered BRIGHT and bold so
/// the eye lands on them first.
fn diff_line_to_ratatui(line: crate::render::diff::DiffLine) -> Line<'static> {
    use crate::render::diff::{LineKind, SpanKind};
    let base = match line.kind {
        LineKind::Removed => palette(tui::WARM),
        LineKind::Added => palette(tui::HOT),
        LineKind::Context => palette(tui::COLD),
    };
    let mut spans =
        vec![Span::styled(format!("      {} ", line.marker()), Style::default().fg(base))];
    for span in &line.spans {
        let style = match span.kind {
            SpanKind::Changed => Style::default()
                .fg(palette(tui::BRIGHT))
                .add_modifier(ratatui::style::Modifier::BOLD),
            SpanKind::Same => Style::default().fg(base),
        };
        spans.push(Span::styled(span.text.clone(), style));
    }
    Line::from(spans)
}

/// Gauge colour by fill. Quiet below two thirds, HOT once compaction is
/// in sight, WARM (red — a crack's colour) once the next turn may prune.
fn ctx_color(pct: u8) -> crossterm::style::Color {
    match pct {
        0..=65 => tui::COLD,
        66..=84 => tui::HOT,
        _ => tui::WARM,
    }
}

/// The `ctx` steer keyword's answer: what the one-word gauge is hiding.
/// The budget is already net of the output reserve and compaction
/// headroom, so "to compaction" is the honest name for the remainder —
/// not "free window".
fn ctx_breakdown(ctx: Option<(u8, u64, usize)>) -> String {
    let Some((pct, used, budget)) = ctx else {
        return "ctx — no reading yet; the gauge fills on the first turn".to_string();
    };
    let left = (budget as u64).saturating_sub(used);
    format!(
        "ctx {pct}% — {} of {} tok, {} to compaction",
        thousands(used),
        thousands(budget as u64),
        thousands(left)
    )
}

/// `104000` → `104,000`. Group-of-three separators, because the whole
/// point of the breakdown is reading magnitudes off at a glance.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn draw_bottom(f: &mut Frame, area: Rect, state: &DashState) {
    let width = area.width as usize;
    let now = state.now();
    // Row 1: what is running now, or where the run stands.
    let (status, status_color) = match forge_status(state, now) {
        Some(s) => {
            let frame = (now.saturating_duration_since(state.started.unwrap_or(now)).as_millis()
                / 250) as usize;
            (format!(" {} {}", tui::spinner_frame(frame), s.trim_start_matches("⚒ ")), palette(tui::HOT))
        }
        None if state.finished => {
            let (counts, _) = dash_counts(state);
            let mut verdict = if counts.cracked > 0 {
                format!(" ✗ {} of {} forged · {} cracked", counts.forged, counts.total, counts.cracked)
            } else if state.goal == Some(false) {
                format!(" ✗ {} of {} forged · goal not met", counts.forged, counts.total)
            } else {
                format!(" █ {} of {} forged", counts.forged, counts.total)
            };
            if state.goal == Some(true) {
                verdict.push_str(" · goal met");
            }
            let failed = counts.cracked > 0 || state.goal == Some(false);
            let color = if failed { palette(tui::WARM) } else { palette(tui::PURE) };
            (verdict, color)
        }
        None if state.turn_started.is_some() => (
            format!(" {} {}…", tui::spinner_frame(0), tui::forge_verb(state.turn)),
            palette(tui::HOT),
        ),
        // Ingots on the anvil with no turn open: the forge is running a
        // proof or a judge between turns, which is still work.
        None if forging_count(state) > 0 => {
            let ids: Vec<&str> = state
                .ingots
                .iter()
                .filter(|r| r.status == IngotStatus::Forging)
                .map(|r| r.id.as_str())
                .collect();
            (format!(" ⚒ checking {}", ids.join(" ")), palette(tui::HOT))
        }
        None if !state.finished && (state.turn > 0 || !state.feed.is_empty()) => {
            (" · between turns".to_string(), palette(tui::COLD))
        }
        None => (" · waiting for the first turn".to_string(), palette(tui::COLD)),
    };
    let mut top = vec![Span::styled(status.clone(), Style::default().fg(status_color))];
    if let Some((pct, _, _)) = state.ctx {
        let ctx = format!("ctx {pct}% ");
        let pad = width.saturating_sub(status.chars().count() + ctx.len());
        top.push(Span::raw(" ".repeat(pad)));
        top.push(Span::styled(ctx, Style::default().fg(palette(ctx_color(pct)))));
    }
    let mut lines = vec![Line::from(top)];
    lines.extend(queued_lines(&state.queued));

    // Row 3: the input, with a placeholder saying what it is for.
    let mut input_spans = vec![Span::styled(" › ", Style::default().fg(palette(tui::HOT)))];
    if state.input.is_empty() {
        input_spans.push(Span::styled("▏", Style::default().fg(palette(tui::BRIGHT))));
        input_spans.push(Span::styled(
            if state.finished { PLACEHOLDER_DONE } else { PLACEHOLDER },
            Style::default().fg(palette(tui::COLD)),
        ));
    } else {
        input_spans.push(Span::styled(state.input.clone(), Style::default().fg(palette(tui::PURE))));
        input_spans.push(Span::styled("▏", Style::default().fg(palette(tui::BRIGHT))));
    }
    lines.push(Line::from(input_spans));

    // Row 4: the keys. An armed Ctrl-C replaces them: for its 800ms it is
    // the only thing worth reading, in a crack's colour.
    let armed = state
        .pending_cancel
        .is_some_and(|t| state.now().saturating_duration_since(t) < DOUBLE_PRESS);
    let hint = if armed {
        Span::styled(" press ^C again to cancel the forge", Style::default().fg(palette(tui::WARM)))
    } else {
        Span::styled(
            format!(" {}", preview(if state.finished { HINT_DONE } else { HINT }, width.saturating_sub(2))),
            Style::default().fg(palette(tui::COLD)),
        )
    };
    lines.push(Line::from(hint));
    f.render_widget(Paragraph::new(lines), area);
}

/// The pending-steer list that sits above the input: dim, oldest first,
/// capped at `QUEUE_SHOWN` with a `+N more` tail. Empty when nothing is
/// pending, so the bar keeps its usual height on a quiet forge.
fn queued_lines(queued: &[String]) -> Vec<Line<'static>> {
    if queued.is_empty() {
        return Vec::new();
    }
    let dim = Style::default().fg(palette(tui::COLD));
    let mut lines: Vec<Line<'static>> = queued
        .iter()
        .take(QUEUE_SHOWN)
        .map(|q| Line::from(Span::styled(format!("  ⏳ {}", preview(q, 64)), dim)))
        .collect();
    if let Some(extra) = queued.len().checked_sub(QUEUE_SHOWN).filter(|n| *n > 0) {
        lines.push(Line::from(Span::styled(format!("  ⏳ +{extra} more"), dim)));
    }
    lines
}

/// How many rows the queued-steer list is allowed before it collapses
/// into `+N more`. The bottom bar grows to fit it, so an unbounded list
/// would eat the crucible and the feed.
pub(crate) const QUEUE_SHOWN: usize = 3;

/// Bottom-bar height: totals + queued list + input + hint.
pub(crate) fn bottom_height(state: &DashState) -> u16 {
    3 + queued_lines(&state.queued).len() as u16
}

// ---------------------------------------------------------------- terminal

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(io::stderr(), LeaveAlternateScreen, crossterm::cursor::Show);
    // Terminal chrome must not outlive the view: a stale `⚒ slag 3/9`
    // title or taskbar pip after exit (or a panic — the hook lands here
    // too) reads as a forge that never finished.
    progress::clear_forge_state();
}

/// Hand the terminal back to the central cleanup registry, which the
/// panic hook and the shell Ctrl-C handler both drain. Registered last,
/// so it runs first: a crucible rescue that prints an error into a
/// dying alternate screen prints into nothing.
///
/// Idempotent by construction — `restore_terminal` is a sequence of
/// "put it back" calls that no-op when the terminal is already sane, so
/// the normal exit path calling it directly costs nothing.
fn register_terminal_restore() {
    crate::shutdown::register(restore_terminal);
}

/// Crossterm input reader on a dedicated thread (crossterm's async
/// `EventStream` needs the `event-stream` feature; a 100ms poll loop
/// gives the same select-able channel with zero new deps). The thread
/// exits within one poll tick of `stop` being set.
fn spawn_key_reader(stop: Arc<AtomicBool>) -> UnboundedReceiver<Event> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(ev) => {
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
    rx
}

/// Run the full-screen dashboard until the user detaches or cancels.
///
/// Renders on stderr (stdout stays a clean log pipe), silences the
/// stream-mode tui while active, and restores the terminal on every exit
/// path — including panics, via the installed hook.
pub async fn run(
    mut rx: UnboundedReceiver<EngineEvent>,
    steer: SteerQueue,
    cancel: CancelFlag,
) -> io::Result<Option<String>> {
    tui::set_quiet(true);
    register_terminal_restore();

    let setup = (|| -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
        enable_raw_mode()?;
        crossterm::execute!(io::stderr(), EnterAlternateScreen)?;
        Terminal::new(CrosstermBackend::new(io::stderr()))
    })();
    let mut terminal = match setup {
        Ok(t) => t,
        Err(e) => {
            restore_terminal();
            tui::set_quiet(false);
            return Err(e);
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let mut keys = spawn_key_reader(stop.clone());

    // Whole-job cost state: seed this invocation's totals from the
    // persisted record so a resumed run's Σ (and the record it saves
    // back) covers the whole job, not just this invocation. The run id
    // is recomputed at every save: on a fresh commission the crucible
    // does not exist yet at this point, and its header (and so the id)
    // only appears once the founder writes PLAN.md.
    let plan = std::fs::read_to_string(CRUCIBLE).unwrap_or_default();
    let run_id = run_id_for_plan(&plan);
    let plan_total =
        plan.lines().filter(|l| l.trim_start().starts_with("(ingot ")).count();
    let mut state = DashState {
        started: Some(Instant::now()),
        commission: std::fs::read_to_string(crate::config::ORE_FILE)
            .ok()
            .and_then(|prd| commission_line(&prd)),
        ..Default::default()
    };
    // Seed the crucible from the plan rather than waiting for events. The
    // pane is a view of PLAN.md, but rows only ever appeared when an
    // ingot started forging, so a run with nothing left to forge -- a
    // finished project, or an addendum that needs no work -- showed an
    // empty crucible beside a feed full of activity.
    state.seed_from_plan(&plan);
    if persistable(&run_id) {
        if let Some(prior) = load_session_cost(&run_id) {
            state.totals = prior.to_usage();
        }
    }
    // Past steers for the Up arrow, and the flush that lands this run's on
    // the way out. Registered rather than called at the end of `run`,
    // because the exit worth protecting is the one that never reaches it.
    state.history = steer_history::recall();
    steer_history::install_flush();

    let result =
        event_loop(&mut terminal, &mut state, plan_total, &mut rx, &mut keys, &steer, &cancel)
            .await;

    save_session_cost(&current_run_id(), &CostRecord::from_usage(&state.totals));
    stop.store(true, Ordering::Relaxed);
    restore_terminal();
    tui::set_quiet(false);
    eprintln!("  dashboard detached, forge continues");
    result
}

/// The commission as one line: PRD.md's last non-empty line, which is
/// where `fire_furnace` writes it.
pub(crate) fn commission_line(prd: &str) -> Option<String> {
    prd.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
}

/// PLAN.md, re-read when its modification time moves. Checked on the draw
/// tick, so a stat call every 33ms and a read only when the file changed.
#[derive(Default)]
struct PlanWatch {
    seen: Option<std::time::SystemTime>,
}

impl PlanWatch {
    fn changed(&mut self) -> Option<String> {
        let modified = std::fs::metadata(CRUCIBLE).and_then(|m| m.modified()).ok()?;
        if self.seen == Some(modified) {
            return None;
        }
        self.seen = Some(modified);
        std::fs::read_to_string(CRUCIBLE).ok()
    }
}

/// The engine is gone: no smith reads the steer queue, and the input
/// takes the next commission instead. Shared by the live loop (channel
/// closed) and a replay (tape ran out), so both end on the same frame.
pub(crate) fn mark_finished(state: &mut DashState) {
    state.finished = true;
    state.push_feed(
        palette(tui::BRIGHT),
        "■ forge finished".into(),
    );
}

// ---------------------------------------------------------------- replay

/// A dashboard state for playing a tape: seeded from the tape's first
/// plan, on the tape's clock, with no disk reads and no saved spend.
pub(crate) fn replay_state(commission: Option<&str>, start: Instant) -> DashState {
    DashState {
        clock: Some(start),
        started: Some(start),
        commission: commission.and_then(commission_line),
        ..Default::default()
    }
}

/// Render one frame into a `w`×`h` grid and return it as plain text rows,
/// trailing blanks trimmed. Deterministic: the same state draws the same
/// rows, which is what lets a replay frame stand in a test.
pub(crate) fn render_text(state: &DashState, w: u16, h: u16) -> Vec<String> {
    use ratatui::backend::TestBackend;
    let Ok(mut terminal) = Terminal::new(TestBackend::new(w.max(1), h.max(1))) else {
        return Vec::new();
    };
    let _ = terminal.draw(|f| draw(f, state));
    let buf = terminal.backend().buffer();
    let mut rows = Vec::with_capacity(h as usize);
    for y in 0..buf.area.height {
        let mut row = String::new();
        let mut skip = 0usize;
        for x in 0..buf.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            // A wide glyph owns the next cell too; the backend pads it
            // with a blank that would double every emoji's width.
            skip = unicode_cells(sym).saturating_sub(1);
            row.push_str(sym);
        }
        rows.push(row.trim_end().to_string());
    }
    rows
}

fn unicode_cells(s: &str) -> usize {
    ratatui::text::Span::raw(s).width().max(1)
}

/// Enter the alternate screen for a replay. Same setup as a live attach,
/// minus the steer history and the saved spend.
pub(crate) fn replay_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    register_terminal_restore();
    enable_raw_mode()?;
    crossterm::execute!(io::stderr(), EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(io::stderr()))
}

/// Draw one replay frame, synchronized where the terminal allows it.
pub(crate) fn replay_draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    state: &DashState,
) -> io::Result<()> {
    let sync = tui::sync_updates_enabled();
    if sync {
        let _ = crossterm::execute!(io::stderr(), crossterm::terminal::BeginSynchronizedUpdate);
    }
    let drawn = terminal.draw(|f| draw(f, state)).map(|_| ());
    if sync {
        let _ = crossterm::execute!(io::stderr(), crossterm::terminal::EndSynchronizedUpdate);
    }
    drawn
}

pub(crate) fn replay_restore() {
    restore_terminal();
}

#[allow(clippy::too_many_arguments)]
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    state: &mut DashState,
    plan_total: usize,
    rx: &mut UnboundedReceiver<EngineEvent>,
    keys: &mut UnboundedReceiver<Event>,
    steer: &SteerQueue,
    cancel: &CancelFlag,
) -> io::Result<Option<String>> {
    let mut interval = tokio::time::interval(FRAME);
    let mut plan_watch = PlanWatch::default();
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut engine_done = false;

    loop {
        tokio::select! {
            ev = rx.recv(), if !engine_done => {
                match ev {
                    Some(ev) => {
                        let mut ingot_ev = is_ingot_event(&ev);
                        let mut cracked = is_crack(&ev);
                        apply_event(state, ev);
                        // Coalesce bursts into one redraw.
                        while let Ok(ev) = rx.try_recv() {
                            ingot_ev |= is_ingot_event(&ev);
                            cracked |= is_crack(&ev);
                            apply_event(state, ev);
                        }
                        if ingot_ev {
                            // Live forge state → terminal title + taskbar
                            // pip. The plan's true size keeps the ratio's
                            // denominator from growing as rows appear.
                            let (mut counts, active) = dash_counts(state);
                            counts.total = counts.total.max(plan_total);
                            progress::report_forge_state(&counts, active.as_deref());
                        }
                        if cracked {
                            // A crack may precede an ugly exit: flush the
                            // whole-job spend now, not just at detach.
                            save_session_cost(&current_run_id(), &CostRecord::from_usage(&state.totals));
                        }
                        dirty = true;
                    }
                    None => {
                        engine_done = true;
                        mark_finished(state);
                        dirty = true;
                    }
                }
            }
            key = keys.recv() => {
                match key {
                    Some(Event::Key(k)) => match handle_key(state, k, steer, cancel) {
                        KeyOutcome::Stay => dirty = true,
                        KeyOutcome::Detach | KeyOutcome::Cancel => return Ok(None),
                        KeyOutcome::Commission(next) => return Ok(Some(next)),
                    },
                    Some(Event::Resize(..)) => dirty = true,
                    Some(_) => {}
                    None => return Ok(None), // input thread died; leave cleanly
                }
            }
            _ = interval.tick() => {
                // A mold cast mid-run reaches no event; the file says it.
                if let Some(plan) = plan_watch.changed() {
                    state.absorb_plan(&plan);
                    dirty = true;
                }
                // Stalled rows change appearance with no event arriving:
                // keep the "(stalled Ns)" counter ticking on screen.
                if has_stalled(&state, Instant::now()) {
                    dirty = true;
                }
                // Same for the bottom-bar spinner status: its elapsed
                // seconds tick with no event arriving.
                if forge_status(state, Instant::now()).is_some() {
                    dirty = true;
                }
                if dirty {
                    // DEC 2026 synchronized output: capable terminals
                    // buffer the frame between BSU/ESU and blit it whole
                    // — no half-drawn panes on slow links. ESU always
                    // follows BSU, even when the draw errors, so the
                    // terminal never sticks in sync mode.
                    let sync = tui::sync_updates_enabled();
                    if sync {
                        let _ = crossterm::execute!(
                            io::stderr(),
                            crossterm::terminal::BeginSynchronizedUpdate
                        );
                    }
                    let drawn = terminal.draw(|f| draw(f, state));
                    if sync {
                        let _ = crossterm::execute!(
                            io::stderr(),
                            crossterm::terminal::EndSynchronizedUpdate
                        );
                    }
                    drawn?;
                    dirty = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn queue() -> (SteerQueue, CancelFlag) {
        (SteerQueue::default(), CancelFlag::default())
    }

    #[test]
    fn apply_event_tracks_ingots_totals_and_feed() {
        let mut state = DashState::default();
        for ev in [
            EngineEvent::IngotStart { id: "i1".into(), work: "build the dashboard".into() },
            EngineEvent::TurnStart { turn: 1 },
            tool_result("bash", true, "ok"),
            EngineEvent::Tokens {
                usage: Usage { total_tokens: 42, cost: Some(0.01), ..Default::default() },
            },
            EngineEvent::Tokens {
                usage: Usage { total_tokens: 8, cost: Some(0.002), ..Default::default() },
            },
            EngineEvent::IngotDone { id: "i1".into(), ok: true },
        ] {
            apply_event(&mut state, ev);
        }

        assert_eq!(state.ingots.len(), 1);
        assert_eq!(state.ingots[0].id, "i1");
        assert_eq!(state.ingots[0].status, IngotStatus::Forged);
        assert_eq!(state.totals.total_tokens, 50);
        assert!((state.totals.cost.unwrap() - 0.012).abs() < 1e-9);
        // Token events fold into the totals without a row of their own:
        // the ingot, its turn, the tool, and the verdict.
        assert_eq!(state.feed.len(), 4);
        assert!(state.feed.back().unwrap().text.contains("i1 forged"));
    }


    #[test]
    fn a_turn_header_carries_its_route_instead_of_two_more_lines() {
        let mut state = DashState::default();
        for ev in [
            EngineEvent::TurnStart { turn: 4 },
            EngineEvent::ModelCall { model: "openrouter/auto".into() },
            EngineEvent::ModelRouted {
                requested: "openrouter/auto".into(),
                routed: "deepseek/deepseek-v4-flash-0731".into(),
            },
        ] {
            apply_event(&mut state, ev);
        }
        assert_eq!(state.feed.len(), 1, "three events collapse to one header row");
        let row = &state.feed[0].text;
        assert!(row.contains("turn 4"), "{row}");
        assert!(row.contains("deepseek-v4-flash-0731"), "{row}");
    }

    #[test]
    fn an_unchanged_route_is_not_repeated_on_later_turns() {
        let mut state = DashState::default();
        for turn in 1..=3 {
            apply_event(&mut state, EngineEvent::TurnStart { turn });
            apply_event(
                &mut state,
                EngineEvent::ModelRouted {
                    requested: "openrouter/auto".into(),
                    routed: "deepseek/v4".into(),
                },
            );
        }
        assert_eq!(state.feed.len(), 3, "one row per turn, never three");
        assert!(state.feed[0].text.contains("deepseek/v4"), "first turn names the route");
        assert!(
            !state.feed[2].text.contains("deepseek/v4"),
            "an unchanged route is not repeated: {}",
            state.feed[2].text
        );
    }

    #[test]
    fn a_changed_route_is_announced_again() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::TurnStart { turn: 1 });
        apply_event(
            &mut state,
            EngineEvent::ModelRouted { requested: "a".into(), routed: "deepseek/v4".into() },
        );
        apply_event(&mut state, EngineEvent::TurnStart { turn: 2 });
        apply_event(
            &mut state,
            EngineEvent::ModelRouted { requested: "a".into(), routed: "qwen/max".into() },
        );
        assert!(state.feed[1].text.contains("qwen/max"), "a switch is news: {}", state.feed[1].text);
    }

    #[test]
    fn a_tool_result_completes_its_call_row_in_place() {
        let mut state = DashState::default();
        apply_event(
            &mut state,
            EngineEvent::ToolCallStart { name: "read_file".into(), preview: "src/main.ts".into() },
        );
        assert_eq!(state.feed.len(), 1);
        apply_event(&mut state, tool_result("read_file", true, "body"));
        assert_eq!(state.feed.len(), 1, "the result completes the call row, it does not add one");
        let row = &state.feed[0].text;
        assert!(row.starts_with("✓"), "completed row carries the result glyph: {row}");
        assert!(row.contains("src/main.ts"), "the call's argument survives: {row}");
    }

    #[test]
    fn parallel_calls_of_one_tool_pair_oldest_first() {
        let mut state = DashState::default();
        for path in ["a.ts", "b.ts"] {
            apply_event(
                &mut state,
                EngineEvent::ToolCallStart { name: "read_file".into(), preview: path.into() },
            );
        }
        apply_event(&mut state, tool_result("read_file", true, "first"));
        assert_eq!(state.feed.len(), 2, "two calls stay two rows");
        assert!(state.feed[0].text.starts_with("✓"), "oldest call completes first");
        assert!(state.feed[1].text.starts_with("→"), "the second is still running");
    }

    #[test]
    fn a_result_with_no_pending_call_still_gets_its_own_row() {
        let mut state = DashState::default();
        apply_event(&mut state, tool_result("bash", true, "out"));
        assert_eq!(state.feed.len(), 1, "an orphan result is never swallowed");
        assert!(state.feed[0].text.starts_with("✓"));
    }

    #[test]
    fn a_failed_result_completes_its_row_with_the_error() {
        let mut state = DashState::default();
        apply_event(
            &mut state,
            EngineEvent::ToolCallStart { name: "bash".into(), preview: "npx tsc".into() },
        );
        apply_event(&mut state, tool_result("bash", false, "type error"));
        assert_eq!(state.feed.len(), 1);
        assert!(state.feed[0].text.starts_with("✗"), "{}", state.feed[0].text);
        assert!(state.feed[0].text.contains("type error"), "{}", state.feed[0].text);
    }

    #[test]
    fn a_long_row_wraps_instead_of_falling_off_the_pane() {
        let row = "■ Built render core. Added shadowMapSize and toneMappingExposure to config.ts";
        let out = wrap_row(row, 30);
        assert!(out.len() > 1, "a row past the pane width wraps");
        assert!(out.iter().all(|l| l.chars().count() <= 30), "no chunk exceeds the width: {out:?}");
        assert_eq!(out.join(" "), row, "wrapping loses nothing");
    }

    #[test]
    fn wrap_row_breaks_a_word_longer_than_the_pane() {
        let out = wrap_row("/a/very/long/path/with/no/spaces/at/all/in/it.ts", 12);
        assert!(out.iter().all(|l| l.chars().count() <= 12), "{out:?}");
        assert_eq!(out.concat(), "/a/very/long/path/with/no/spaces/at/all/in/it.ts");
    }

    #[test]
    fn wrap_row_respects_char_boundaries() {
        // Multibyte must never split mid-character.
        let out = wrap_row("héllo wörld ünïcode ✓ forged", 10);
        assert!(out.iter().all(|l| l.chars().count() <= 10), "{out:?}");
        assert_eq!(out.join(" "), "héllo wörld ünïcode ✓ forged");
    }

    #[test]
    fn a_pane_too_narrow_to_wrap_returns_the_row_whole() {
        assert_eq!(wrap_row("some text", 3), vec!["some text".to_string()]);
    }

    #[test]
    fn a_turn_header_names_its_ingot_so_a_reset_counter_reads_right() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i3".into(), work: "arena".into() });
        apply_event(&mut state, EngineEvent::TurnStart { turn: 1 });
        let head = &state.feed.back().unwrap().text;
        assert!(head.contains("i3 · turn 1"), "the ingot is named: {head}");
    }

    #[test]
    fn a_turn_is_credited_to_the_ingot_still_forging_not_the_last_started() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "a".into() });
        apply_event(&mut state, EngineEvent::IngotStart { id: "i2".into(), work: "b".into() });
        apply_event(&mut state, EngineEvent::IngotDone { id: "i2".into(), ok: true });
        apply_event(&mut state, EngineEvent::TurnStart { turn: 4 });
        let head = &state.feed.back().unwrap().text;
        assert!(head.contains("i1 · turn 4"), "{head}");
    }

    #[test]
    fn a_turn_before_any_ingot_keeps_a_bare_header() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::TurnStart { turn: 1 });
        assert_eq!(state.feed.back().unwrap().text, "⚒ turn 1");
    }

    #[test]
    fn a_crucible_row_is_named_by_its_work_not_by_forged() {
        let mut state = DashState::default();
        apply_event(
            &mut state,
            EngineEvent::IngotStart {
                id: "i1".into(),
                work: "Procedurally generate a modular arena".into(),
            },
        );
        apply_event(&mut state, EngineEvent::IngotDone { id: "i1".into(), ok: true });
        let line = ingot_line(&state.ingots[0], Instant::now(), 50);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Procedurally generate"), "the work names the row: {text}");
        assert!(!text.contains("forged"), "the glyph already says forged: {text}");
    }

    #[test]
    fn a_stalled_row_keeps_its_title_and_gains_the_stall() {
        let mut state = DashState::default();
        apply_event(
            &mut state,
            EngineEvent::IngotStart { id: "i10".into(), work: "WebAudio engine".into() },
        );
        state.ingots[0].last_activity = Instant::now() - STALL_WARN - Duration::from_secs(1);
        let line = ingot_line(&state.ingots[0], Instant::now(), 50);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("WebAudio engine"), "{text}");
        assert!(text.contains("stalled"), "a stall is news the glyph cannot carry: {text}");
    }

    #[test]
    fn a_row_with_no_work_yet_falls_back_to_its_status() {
        let row = IngotRow {
            id: "i2".into(),
            work: String::new(),
            heat: 0,
            status: IngotStatus::Forged,
            last_activity: Instant::now(),
        };
        let line = ingot_line(&row, Instant::now(), 50);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("forged"), "an unnamed row still says something: {text}");
    }

    #[test]
    fn ingot_title_drops_the_plan_label_and_breaks_on_a_word() {
        assert_eq!(ingot_title("GOAL: Build the arena", 40), "Build the arena");
        assert_eq!(ingot_title("task: lower case label", 40), "lower case label");
        let t = ingot_title("Procedurally generate a modular arena with walls", 24);
        assert!(t.chars().count() <= 24, "{t}");
        assert!(t.ends_with('…'), "{t}");
        assert!(!t.contains("  "), "{t}");
        assert!(t.starts_with("Procedurally"), "{t}");
    }

    #[test]
    fn ingot_title_survives_a_pane_with_no_room() {
        assert_eq!(ingot_title("anything at all", 1), "…");
        assert_eq!(ingot_title("", 20), "");
    }

    #[test]
    fn a_steer_after_the_forge_ends_becomes_the_next_commission() {
        // The bug: Enter pushed into a SteerQueue with no reader once the
        // engine channel closed, so the keystroke vanished and a finished
        // screen was a full stop.
        let (steer, cancel) = queue();
        let mut state = DashState::default();
        state.finished = true;
        state.input = "now add multiplayer".into();
        let out = handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(out, KeyOutcome::Commission("now add multiplayer".to_string()));
        assert!(
            steer.lock().unwrap().is_empty(),
            "a finished run must not queue a steer nobody drains"
        );
        assert!(state.input.is_empty(), "the input clears for the next forge");
    }

    #[test]
    fn a_steer_during_a_live_forge_still_steers() {
        let (steer, cancel) = queue();
        let mut state = DashState::default();
        state.input = "focus the tests".into();
        let out = handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(out, KeyOutcome::Stay, "a live run steers, it does not re-commission");
        assert_eq!(steer.lock().unwrap().as_slice(), ["focus the tests".to_string()]);
    }

    #[test]
    fn a_new_commission_joins_the_steer_history() {
        let (steer, cancel) = queue();
        let mut state = DashState::default();
        state.finished = true;
        state.input = "add a leaderboard".into();
        handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(state.history.first().map(String::as_str), Some("add a leaderboard"));
    }

    #[test]
    fn an_empty_input_never_commissions_anything() {
        let (steer, cancel) = queue();
        let mut state = DashState::default();
        state.finished = true;
        let out = handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(out, KeyOutcome::Stay, "Enter on an empty box is not a commission");
    }

    #[test]
    fn the_hint_stops_offering_a_steer_once_there_is_no_smith() {
        assert!(HINT.contains("steer"));
        assert!(HINT_DONE.contains("commission"), "{HINT_DONE}");
        assert!(!HINT_DONE.contains("steer the"), "no smith left to steer: {HINT_DONE}");
    }

    #[test]
    fn the_crucible_shows_the_plan_before_any_event_arrives() {
        // A finished project forges nothing, so no ingot event ever fires
        // and the pane used to sit empty beside a busy feed.
        let mut state = DashState::default();
        state.seed_from_plan(
            "(ingot :id \"i1\" :status forged :solo t :grade 1 :heat 0 :max 5 :proof \"true\" :work \"render core\")\n\
             (ingot :id \"i2\" :status cracked :solo t :grade 1 :heat 2 :max 5 :proof \"true\" :work \"audio\")\n\
             (ingot :id \"i3\" :status ore :solo t :grade 1 :heat 0 :max 5 :proof \"true\" :work \"netcode\")",
        );
        assert_eq!(state.ingots.len(), 3);
        assert_eq!(state.ingots[0].status, IngotStatus::Forged);
        assert_eq!(state.ingots[0].work, "render core");
        assert_eq!(state.ingots[1].status, IngotStatus::Cracked);
        assert_eq!(state.ingots[1].heat, 2);
        assert_eq!(
            state.ingots.iter().filter(|r| r.status == IngotStatus::Forged).count(),
            1
        );
    }

    #[test]
    fn seeding_leaves_later_events_in_charge() {
        let mut state = DashState::default();
        state.seed_from_plan(
            "(ingot :id \"i1\" :status ore :solo t :grade 1 :heat 0 :max 5 :proof \"true\" :work \"netcode\")",
        );
        apply_event(&mut state, EngineEvent::IngotDone { id: "i1".into(), ok: true });
        assert_eq!(state.ingots.len(), 1, "the event updates the seeded row, it does not add one");
        assert_eq!(state.ingots[0].status, IngotStatus::Forged);
    }

    #[test]
    fn an_empty_plan_seeds_nothing() {
        let mut state = DashState::default();
        state.seed_from_plan("");
        assert!(state.ingots.is_empty());
    }

    #[test]
    fn heat_and_duel_events_update_rows() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i2".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::HeatTick { id: "i2".into(), heat: 3 });
        assert_eq!(state.ingots[0].heat, 3);
        assert_eq!(state.ingots[0].status, IngotStatus::Forging);

        apply_event(&mut state, EngineEvent::DuelRound { id: "i2".into(), round: 2 });
        assert_eq!(state.ingots[0].status, IngotStatus::Duel(2));

        apply_event(
            &mut state,
            EngineEvent::DuelVerdict { id: "i2".into(), winner: 'a', margin: 12 },
        );
        assert_eq!(state.ingots[0].status, IngotStatus::Verdict { winner: 'a', margin: 12 });

        // Events for an unseen ingot create its row (out-of-order safety).
        apply_event(&mut state, EngineEvent::IngotDone { id: "i9".into(), ok: false });
        assert_eq!(state.ingots.len(), 2);
        assert_eq!(state.ingots[1].status, IngotStatus::Cracked);
    }

    #[test]
    fn tokens_and_tool_results_refresh_only_forging_rows() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::IngotStart { id: "i2".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::IngotDone { id: "i2".into(), ok: true });

        // Backdate both clocks; skip on platforms where Instant can't go
        // that far back (fresh-boot containers).
        let Some(old) = Instant::now().checked_sub(Duration::from_secs(300)) else { return };
        for row in &mut state.ingots {
            row.last_activity = old;
        }

        apply_event(
            &mut state,
            EngineEvent::Tokens { usage: Usage { total_tokens: 1, ..Default::default() } },
        );
        assert_ne!(state.ingots[0].last_activity, old, "forging row must refresh");
        assert_eq!(state.ingots[1].last_activity, old, "forged row must not refresh");

        state.ingots[0].last_activity = old;
        apply_event(
            &mut state,
            tool_result("bash", false, "x"),
        );
        assert_ne!(state.ingots[0].last_activity, old);
    }

    #[test]
    fn stalled_forging_rows_tint_yellow_then_red() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        let row = &state.ingots[0];
        let base = row.last_activity;

        let text_of = |line: &Line| -> String {
            line.spans.iter().map(|s| s.content.clone()).collect()
        };

        // Fresh: heat color, no stall suffix.
        let fresh = ingot_line(row, base, 50);
        assert!(!text_of(&fresh).contains("stalled"));

        // 20s of silence: yellow, "(stalled 20s)".
        let warn = ingot_line(row, base + Duration::from_secs(20), 50);
        assert!(text_of(&warn).contains("stalled 20s"), "{}", text_of(&warn));
        assert_eq!(warn.spans[0].style.fg, Some(palette(tui::BRIGHT)));

        // 90s of silence: red.
        let dead = ingot_line(row, base + Duration::from_secs(90), 50);
        assert!(text_of(&dead).contains("stalled 90s"), "{}", text_of(&dead));
        assert_eq!(dead.spans[0].style.fg, Some(palette(tui::WARM)));
    }

    #[test]
    fn stall_tint_only_applies_to_forging_rows() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::IngotDone { id: "i1".into(), ok: true });
        let row = &state.ingots[0];
        let line = ingot_line(row, row.last_activity + Duration::from_secs(90), 50);
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert!(!text.contains("stalled"), "forged row must never show a stall");
        // The row is named by its work; `forged` is carried by the glyph
        // and its colour, so assert the signal itself rather than a word.
        assert_eq!(line.spans[0].content.as_ref(), "✓ ");
        assert_eq!(line.spans[0].style.fg, Some(palette(tui::PURE)));
    }

    #[test]
    fn has_stalled_detects_silent_forging_rows() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        let base = state.ingots[0].last_activity;

        assert!(!has_stalled(&state, base + Duration::from_secs(5)));
        assert!(has_stalled(&state, base + STALL_WARN));

        // Done rows never count as stalled.
        state.ingots[0].status = IngotStatus::Cracked;
        assert!(!has_stalled(&state, base + Duration::from_secs(500)));
    }

    #[test]
    fn forge_status_folds_turn_tokens_and_elapsed_into_the_live_line() {
        let mut state = DashState::default();
        // No turn yet: no status line.
        assert_eq!(forge_status(&state, Instant::now()), None);

        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::TurnStart { turn: 0 });
        apply_event(
            &mut state,
            EngineEvent::Tokens {
                usage: Usage { total_tokens: 4100, ..Default::default() },
            },
        );

        let t0 = state.turn_started.unwrap();
        let line = forge_status(&state, t0 + Duration::from_secs(12)).unwrap();
        // Esc detaches, it does not interrupt; the line no longer says so.
        assert_eq!(line, "⚒ Forging… i1 · 12s · 4.1k tok · 342 tok/s");

        // A new turn re-zeros the token fold and rotates the verb.
        apply_event(&mut state, EngineEvent::TurnStart { turn: 1 });
        assert_eq!(state.turn_tokens, 0);
        let t1 = state.turn_started.unwrap();
        let line = forge_status(&state, t1 + Duration::from_secs(2)).unwrap();
        assert!(line.starts_with("⚒ Smelting… i1 · 2s · 0 tok"), "{line}");
        assert!(!line.contains("tok/s"), "rate guarded below 5s/2000 tok: {line}");
    }

    #[test]
    fn forge_status_disappears_when_nothing_is_forging_or_the_run_ends() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::TurnStart { turn: 3 });
        assert!(forge_status(&state, Instant::now()).is_some());

        // All rows done: totals take the bar back.
        apply_event(&mut state, EngineEvent::IngotDone { id: "i1".into(), ok: true });
        assert_eq!(forge_status(&state, Instant::now()), None);

        // Finish/Error clear the turn clock even with a forging row left.
        apply_event(&mut state, EngineEvent::IngotStart { id: "i2".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::TurnStart { turn: 4 });
        apply_event(&mut state, EngineEvent::Error { message: "boom".into() });
        assert_eq!(forge_status(&state, Instant::now()), None);
    }

    #[test]
    fn bottom_bar_shows_the_spinner_status_while_forging() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::TurnStart { turn: 0 });
        terminal.draw(|f| draw(f, &state)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("Forging…"), "spinner status must render");
        assert!(!content.contains("esc to interrupt"), "Esc detaches; the bar must not promise more");
    }

    #[test]
    fn feed_is_capped_at_200() {
        let mut state = DashState::default();
        for turn in 0..300 {
            apply_event(&mut state, EngineEvent::TurnStart { turn });
        }
        assert_eq!(state.feed.len(), FEED_CAP);
        assert!(state.feed.back().unwrap().text.contains("turn 299"));
    }

    #[test]
    fn enter_pushes_input_into_steer_queue_and_flashes() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        for c in "focus".chars() {
            assert_eq!(
                handle_key(&mut state, press(KeyCode::Char(c)), &steer, &cancel),
                KeyOutcome::Stay
            );
        }
        handle_key(&mut state, press(KeyCode::Backspace), &steer, &cancel);
        handle_key(&mut state, press(KeyCode::Char('s')), &steer, &cancel);
        assert_eq!(state.input, "focus");

        handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(steer.lock().unwrap().as_slice(), ["focus".to_string()]);
        assert!(state.input.is_empty());
        assert_eq!(state.queued, vec!["focus".to_string()], "mirrors the live queue");

        // Enter with an empty buffer queues nothing.
        handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(steer.lock().unwrap().len(), 1);
    }

    #[test]
    fn up_walks_the_recall_list_and_down_restores_the_draft() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        // Newest first, the order `steer_history::recall` hands back.
        state.history = vec!["retry the proof".into(), "focus".into()];

        for c in "half-t".chars() {
            handle_key(&mut state, press(KeyCode::Char(c)), &steer, &cancel);
        }
        handle_key(&mut state, press(KeyCode::Up), &steer, &cancel);
        assert_eq!(state.input, "retry the proof", "newest recalls first");
        handle_key(&mut state, press(KeyCode::Up), &steer, &cancel);
        assert_eq!(state.input, "focus");
        // Past the oldest, Up holds rather than emptying the line.
        handle_key(&mut state, press(KeyCode::Up), &steer, &cancel);
        assert_eq!(state.input, "focus");

        handle_key(&mut state, press(KeyCode::Down), &steer, &cancel);
        assert_eq!(state.input, "retry the proof");
        handle_key(&mut state, press(KeyCode::Down), &steer, &cancel);
        assert_eq!(state.input, "half-t", "walking past the newest restores what was typed");
        // Already at the draft: another Down is a no-op, not a clear.
        handle_key(&mut state, press(KeyCode::Down), &steer, &cancel);
        assert_eq!(state.input, "half-t");
    }

    #[test]
    fn submitting_a_recalled_steer_resets_the_cursor_and_prepends_it() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        state.history = vec!["focus".into(), "retry".into()];

        handle_key(&mut state, press(KeyCode::Up), &steer, &cancel);
        handle_key(&mut state, press(KeyCode::Up), &steer, &cancel);
        assert_eq!(state.input, "retry");
        handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);

        assert_eq!(steer.lock().unwrap().as_slice(), ["retry".to_string()]);
        assert_eq!(state.history_pos, None, "the cursor resets on submit");
        assert_eq!(
            state.history[0], "retry",
            "a resent steer becomes the newest, without a duplicate below it"
        );
        assert_eq!(state.history, vec!["retry".to_string(), "focus".to_string()]);

        // The next Up starts from the newest again, not where the last walk stopped.
        handle_key(&mut state, press(KeyCode::Up), &steer, &cancel);
        assert_eq!(state.input, "retry");
    }

    #[test]
    fn typing_after_a_recall_keeps_the_recalled_text_as_the_new_draft() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        state.history = vec!["focus".into()];

        handle_key(&mut state, press(KeyCode::Up), &steer, &cancel);
        handle_key(&mut state, press(KeyCode::Char('!')), &steer, &cancel);
        assert_eq!(state.input, "focus!");
        // Editing ends the walk: Down must not throw the edit away.
        handle_key(&mut state, press(KeyCode::Down), &steer, &cancel);
        assert_eq!(state.input, "focus!");
    }

    #[test]
    fn quit_keys_respect_input_buffer() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();

        assert_eq!(handle_key(&mut state, press(KeyCode::Char('q')), &steer, &cancel), KeyOutcome::Detach);
        assert_eq!(handle_key(&mut state, press(KeyCode::Esc), &steer, &cancel), KeyOutcome::Detach);

        // With text in the buffer, q is a plain character and Esc clears.
        handle_key(&mut state, press(KeyCode::Char('a')), &steer, &cancel);
        assert_eq!(handle_key(&mut state, press(KeyCode::Char('q')), &steer, &cancel), KeyOutcome::Stay);
        assert_eq!(state.input, "aq");
        assert_eq!(handle_key(&mut state, press(KeyCode::Esc), &steer, &cancel), KeyOutcome::Stay);
        assert!(state.input.is_empty());
        assert!(!cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn the_queued_list_caps_at_three_with_a_more_tail() {
        let none: Vec<String> = Vec::new();
        assert!(queued_lines(&none).is_empty(), "a quiet forge keeps the bar at 3 rows");

        let two: Vec<String> = vec!["a".into(), "b".into()];
        assert_eq!(queued_lines(&two).len(), 2, "under the cap, no tail");

        let five: Vec<String> = (0..5).map(|i| format!("steer {i}")).collect();
        let lines = queued_lines(&five);
        assert_eq!(lines.len(), QUEUE_SHOWN + 1);
        let rendered: String =
            lines.iter().flat_map(|l| l.spans.iter()).map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("steer 0") && rendered.contains("steer 2"));
        assert!(!rendered.contains("steer 3"), "past the cap, collapsed");
        assert!(rendered.contains("+2 more"), "got: {rendered}");
    }

    #[test]
    fn a_delivered_steer_leaves_the_queued_list() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        for c in "focus".chars() {
            handle_key(&mut state, press(KeyCode::Char(c)), &steer, &cancel);
        }
        handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(state.queued, vec!["focus".to_string()]);

        // The engine drained it and said so: the row goes away, and the
        // user learns the smith actually has it.
        apply_event(&mut state, EngineEvent::Steer { text: "focus".into() });
        assert!(state.queued.is_empty());
    }

    #[test]
    fn an_unrelated_steer_confirmation_leaves_the_queue_alone() {
        let mut state = DashState::default();
        state.queued = vec!["mine".into()];
        apply_event(&mut state, EngineEvent::Steer { text: "somebody else's".into() });
        assert_eq!(state.queued, vec!["mine".to_string()], "only a match dequeues");
    }

    #[test]
    fn the_bottom_bar_grows_to_fit_the_queued_list() {
        let mut state = DashState::default();
        assert_eq!(bottom_height(&state), 3, "totals + input + hint");
        state.queued = vec!["a".into()];
        assert_eq!(bottom_height(&state), 4);
        state.queued = (0..9).map(|i| i.to_string()).collect();
        assert_eq!(bottom_height(&state), 3 + QUEUE_SHOWN as u16 + 1, "capped, never unbounded");
    }

    fn tool_result(name: &str, ok: bool, preview: &str) -> EngineEvent {
        EngineEvent::ToolResult {
            name: name.into(),
            ok,
            preview: preview.into(),
            lines: preview.lines().count(),
            bytes: preview.len(),
            ms: 300,
        }
    }

    #[test]
    fn ctrl_o_toggles_the_expanded_feed() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert!(!state.expanded, "collapsed by default");
        handle_key(&mut state, key, &steer, &cancel);
        assert!(state.expanded);
        handle_key(&mut state, key, &steer, &cancel);
        assert!(!state.expanded, "it is a toggle");
        assert!(!cancel.load(Ordering::SeqCst), "ctrl-o is not ctrl-c");
    }

    #[test]
    fn the_expand_hint_appears_once_in_the_hint_bar() {
        // Not once per feed line: two hundred results would repeat it.
        assert_eq!(HINT.matches("^O").count(), 1);
    }

    #[test]
    fn a_tool_result_keeps_its_preview_as_expandable_detail() {
        let mut state = DashState::default();
        apply_event(&mut state, tool_result("read_file", true, "line one\nline two"));
        let last = state.feed.back().expect("a feed line");
        assert!(last.text.contains("read"), "{}", last.text);
        assert!(last.text.contains("2 lines"), "collapsed line carries counts: {}", last.text);
        assert_eq!(last.detail.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn a_unified_diff_preview_renders_through_the_word_differ() {
        let detail = " keep me\n-let total = sum(xs);\n+let count = sum(xs);\n";
        let lines = detail_lines(detail);
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect();
        assert!(rendered.iter().any(|l| l.contains("- let total")), "got: {rendered:?}");
        assert!(rendered.iter().any(|l| l.contains("+ let count")), "got: {rendered:?}");
        // The renamed word is the only bold span on the added line.
        let added = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("count")))
            .expect("an added line");
        let bold: Vec<&str> = added
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(ratatui::style::Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bold, vec!["count"]);
    }

    #[test]
    fn plain_output_is_not_mistaken_for_a_diff() {
        let lines = detail_lines("just some output\nno markers here");
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect();
        assert_eq!(rendered, vec!["      just some output", "      no markers here"]);
    }

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_single_ctrl_c_only_arms_the_cancel() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        let t0 = Instant::now();
        assert_eq!(handle_key_at(&mut state, ctrl_c(), &steer, &cancel, t0), KeyOutcome::Stay);
        assert!(!cancel.load(Ordering::SeqCst), "one press must not kill a 20-minute forge");
        assert!(state.pending_cancel.is_some(), "armed");
    }

    #[test]
    fn a_second_ctrl_c_inside_the_window_cancels() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        let t0 = Instant::now();
        handle_key_at(&mut state, ctrl_c(), &steer, &cancel, t0);
        let outcome =
            handle_key_at(&mut state, ctrl_c(), &steer, &cancel, t0 + DOUBLE_PRESS / 2);
        assert_eq!(outcome, KeyOutcome::Cancel);
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn a_lapsed_arm_starts_over_instead_of_cancelling() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        let t0 = Instant::now();
        handle_key_at(&mut state, ctrl_c(), &steer, &cancel, t0);
        // Two strays a full second apart are two accidents, not intent.
        let outcome =
            handle_key_at(&mut state, ctrl_c(), &steer, &cancel, t0 + DOUBLE_PRESS * 2);
        assert_eq!(outcome, KeyOutcome::Stay);
        assert!(!cancel.load(Ordering::SeqCst));
        assert!(state.pending_cancel.is_some(), "re-armed by the later press");
    }

    #[test]
    fn typing_after_a_stray_ctrl_c_disarms_it() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        let t0 = Instant::now();
        handle_key_at(&mut state, ctrl_c(), &steer, &cancel, t0);
        handle_key_at(&mut state, press(KeyCode::Char('x')), &steer, &cancel, t0);
        assert!(state.pending_cancel.is_none());
        // So the *next* Ctrl-C is a first press again, not a pair.
        let outcome = handle_key_at(&mut state, ctrl_c(), &steer, &cancel, t0);
        assert_eq!(outcome, KeyOutcome::Stay);
        assert!(!cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn an_armed_cancel_flashes_the_press_again_hint() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut state = DashState::default();
        state.pending_cancel = Some(Instant::now());
        terminal.draw(|f| draw(f, &state)).unwrap();
        let content: String =
            terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("press ^C again"), "got: {content}");
    }

    #[test]
    fn key_release_is_ignored() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        let mut key = press(KeyCode::Char('q'));
        key.kind = KeyEventKind::Release;
        assert_eq!(handle_key(&mut state, key, &steer, &cancel), KeyOutcome::Stay);
    }

    #[test]
    fn draw_renders_empty_and_populated_state() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let state = DashState::default();
        terminal.draw(|f| draw(f, &state)).unwrap();

        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "forge".into() });
        apply_event(
            &mut state,
            EngineEvent::Tokens {
                usage: Usage { total_tokens: 42, cost: Some(0.01), ..Default::default() },
            },
        );
        state.input = "steer me".into();
        state.queued = vec!["steer me".into()];
        terminal.draw(|f| draw(f, &state)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("i1"));
        // The crucible names a row by its work, not by a column of status
        // words that repeat what the glyph already says.
        assert!(content.contains("forge"));
        assert!(content.contains("42 tok"));
        assert!(content.contains("steer me"));
        // The pending steer persists as its own dim row, not a 1.5s flash.
        // (⏳ is double-width, so the backend pads a cell after it.)
        assert!(content.contains("⏳"), "got: {content}");
        assert_eq!(content.matches("steer me").count(), 2, "queued row + input line");
        // Hint line clips at 80 cols; assert on a segment that survives.
        assert!(content.contains("^O expand"), "one expand hint in the bar, not per line");
    }

    /// Every tui palette color must survive the crossterm → ratatui hop.
    /// `Color::Reset` is the match arm's give-up value: it paints the
    /// terminal default, so a feed line that hit it would lose its meaning.
    #[test]
    fn palette_maps_every_tui_color() {
        for color in [tui::COLD, tui::WARM, tui::HOT, tui::BRIGHT, tui::PURE] {
            let mapped = palette(color);
            assert_ne!(mapped, Color::Reset, "{color:?} fell through to Reset");
        }
    }

    /// Rows fold back into crucible-shaped counts; the newest still-forging
    /// row is the "forging iN" the title names.
    #[test]
    fn dash_counts_folds_rows_and_names_the_active_ingot() {
        let mut state = DashState::default();
        for (id, ev) in [
            ("i1", EngineEvent::IngotStart { id: "i1".into(), work: "w".into() }),
            ("i2", EngineEvent::IngotStart { id: "i2".into(), work: "w".into() }),
            ("i3", EngineEvent::IngotStart { id: "i3".into(), work: "w".into() }),
        ] {
            let _ = id;
            apply_event(&mut state, ev);
        }
        apply_event(&mut state, EngineEvent::IngotDone { id: "i1".into(), ok: true });
        apply_event(&mut state, EngineEvent::IngotDone { id: "i2".into(), ok: false });

        let (counts, active) = dash_counts(&state);
        assert_eq!(counts.total, 3);
        assert_eq!(counts.forged, 1);
        assert_eq!(counts.cracked, 1);
        assert_eq!(counts.molten, 1);
        assert_eq!(active.as_deref(), Some("i3"));

        // A duel row counts as molten but is not "forging" for the title.
        apply_event(&mut state, EngineEvent::DuelRound { id: "i3".into(), round: 1 });
        let (counts, active) = dash_counts(&state);
        assert_eq!(counts.molten, 1);
        assert_eq!(active, None);
    }

    /// One job = one `;; CRUCIBLE <timestamp>` header. Re-smelt splits
    /// change the ingot set but keep the header, so the id survives them;
    /// a freshly cast plan (new timestamp) starts a fresh record.
    #[test]
    fn run_id_hangs_off_the_crucible_header_not_the_ingots() {
        let plan_a = ";; CRUCIBLE 2026-08-18 10:00\n(ingot :id \"i1\" ...)\n";
        let split = ";; CRUCIBLE 2026-08-18 10:00\n(ingot :id \"i1a\" ...)\n(ingot :id \"i1b\" ...)\n";
        let plan_b = ";; CRUCIBLE 2026-08-19 09:00\n(ingot :id \"i1\" ...)\n";

        assert_eq!(run_id_for_plan(plan_a), run_id_for_plan(split), "split keeps the id");
        assert_ne!(run_id_for_plan(plan_a), run_id_for_plan(plan_b), "new plan, new id");
        assert_eq!(run_id_for_plan("no header here"), "default");
        assert_eq!(run_id_for_plan(plan_a).len(), 16, "16 hex chars");
    }

    /// "default" is the no-crucible placeholder, not a job: persisting
    /// under it would misattribute a fresh commission's early spend to
    /// every later job in the directory, and seeding from it would show
    /// prior unrelated jobs' totals. The save path recomputes the id at
    /// save time instead, when PLAN.md (and its header) exists.
    #[test]
    fn default_run_id_is_never_a_persistence_key() {
        assert!(!persistable("default"));
        assert!(persistable(&run_id_for_plan(";; CRUCIBLE 2026-08-18 10:00\n")));
    }

    /// Save → load round-trips through `.slag/session-costs.json`; a second
    /// run id coexists, and re-saving a run replaces its record (the
    /// caller's totals are already cumulative — adding would double-count).
    #[test]
    fn session_costs_round_trip_and_replace_per_run() {
        let dir = std::env::temp_dir()
            .join(format!("slag-costs-{}-{:?}", std::process::id(), std::thread::current().id()));
        let path = dir.join("session-costs.json");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(load_session_cost_at(&path, "r1"), None, "missing file reads empty");

        let first = CostRecord { total_tokens: 4100, cost: Some(0.31), ..Default::default() };
        save_session_cost_at(&path, "r1", &first).unwrap();
        let other = CostRecord { total_tokens: 7, cost: None, ..Default::default() };
        save_session_cost_at(&path, "r2", &other).unwrap();

        assert_eq!(load_session_cost_at(&path, "r1"), Some(first));
        assert_eq!(load_session_cost_at(&path, "r2"), Some(other.clone()));

        // Resume: cumulative record replaces, other runs stay untouched.
        let resumed = CostRecord {
            total_tokens: 9000,
            cost: Some(0.55),
            prompt_tokens: 6000,
            completion_tokens: 3000,
        };
        save_session_cost_at(&path, "r1", &resumed).unwrap();
        assert_eq!(load_session_cost_at(&path, "r1"), Some(resumed));
        assert_eq!(load_session_cost_at(&path, "r2"), Some(other));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Usage ↔ record conversion is lossless — the resumed dashboard's Σ
    /// must equal what the previous invocation persisted.
    #[test]
    fn cost_record_round_trips_usage() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 42,
            total_tokens: 142,
            cost: Some(0.05),
            ..Default::default()
        };
        let rec = CostRecord::from_usage(&usage);
        let back = rec.to_usage();
        assert_eq!(back.prompt_tokens, 100);
        assert_eq!(back.completion_tokens, 42);
        assert_eq!(back.total_tokens, 142);
        assert_eq!(back.cost, Some(0.05));

        assert!(CostRecord::default().is_empty(), "zero spend is not worth a file");
        assert!(!rec.is_empty());
    }

    #[test]
    fn crack_events_are_the_flush_trigger() {
        assert!(is_crack(&EngineEvent::IngotDone { id: "i1".into(), ok: false }));
        assert!(!is_crack(&EngineEvent::IngotDone { id: "i1".into(), ok: true }));
        assert!(is_ingot_event(&EngineEvent::IngotStart { id: "i1".into(), work: "w".into() }));
        assert!(!is_ingot_event(&EngineEvent::TurnStart { turn: 1 }));
    }

    #[test]
    fn gauge_updates_the_bar_without_spending_a_feed_line() {
        let mut state = DashState::default();
        apply_event(
            &mut state,
            EngineEvent::ContextGauge { pct: 48, used_tokens: 96_000, budget_tokens: 200_000 },
        );
        assert_eq!(state.ctx, Some((48, 96_000, 200_000)));
        assert!(state.feed.is_empty(), "the gauge claimed a feed line");
    }

    #[test]
    fn gauge_heats_up_as_compaction_nears() {
        assert_eq!(ctx_color(0), tui::COLD);
        assert_eq!(ctx_color(65), tui::COLD);
        assert_eq!(ctx_color(66), tui::HOT);
        assert_eq!(ctx_color(84), tui::HOT);
        assert_eq!(ctx_color(85), tui::WARM);
        assert_eq!(ctx_color(100), tui::WARM);
    }

    #[test]
    fn ctx_breakdown_names_the_remainder_and_groups_digits() {
        let line = ctx_breakdown(Some((48, 96_000, 200_000)));
        assert_eq!(line, "ctx 48% — 96,000 of 200,000 tok, 104,000 to compaction");
        // Over budget: the remainder floors at zero rather than wrapping.
        let over = ctx_breakdown(Some((100, 210_000, 200_000)));
        assert!(over.ends_with("0 to compaction"), "{over}");
        assert!(ctx_breakdown(None).contains("no reading yet"));
    }

    #[test]
    fn thousands_groups_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn draw_survives_tiny_terminal() {
        let mut terminal = Terminal::new(TestBackend::new(10, 4)).unwrap();
        let mut state = DashState::default();
        for turn in 0..50 {
            apply_event(&mut state, EngineEvent::TurnStart { turn });
        }
        terminal.draw(|f| draw(f, &state)).unwrap();
    }
}
