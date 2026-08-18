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

use std::collections::VecDeque;
use std::io;
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

use crate::engine::events::preview;
use crate::engine::{CancelFlag, EngineEvent, SteerQueue, Usage};
use crate::tui;

/// Rolling feed cap.
const FEED_CAP: usize = 200;
/// Draw coalescing: at most one render per frame (~30fps).
const FRAME: Duration = Duration::from_millis(33);
/// How long "steer queued" stays visible.
const FLASH: Duration = Duration::from_millis(1500);
/// A forging ingot with no tokens/tool activity for this long tints yellow.
const STALL_WARN: Duration = Duration::from_secs(15);
/// … and red after this long.
const STALL_DEAD: Duration = Duration::from_secs(60);

const HINT: &str =
    "type+Enter: steer the smith · Esc/q (empty input): quit view · Ctrl-C: cancel forge";

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
    pub(crate) color: Color,
    pub(crate) text: String,
}

#[derive(Debug, Default)]
pub(crate) struct DashState {
    pub(crate) ingots: Vec<IngotRow>,
    pub(crate) feed: VecDeque<FeedLine>,
    pub(crate) totals: Usage,
    pub(crate) input: String,
    pub(crate) flash_until: Option<Instant>,
    /// Current turn number; picks the metallurgical verb.
    pub(crate) turn: usize,
    /// When the current turn started — the live status line's clock.
    /// `None` between turns and after Finish/Error.
    pub(crate) turn_started: Option<Instant>,
    /// Tokens folded since the turn started (EngineEvent::Tokens deltas).
    pub(crate) turn_tokens: u64,
}

impl DashState {
    fn row_mut(&mut self, id: &str) -> &mut IngotRow {
        if let Some(i) = self.ingots.iter().position(|r| r.id == id) {
            return &mut self.ingots[i];
        }
        self.ingots.push(IngotRow {
            id: id.to_string(),
            work: String::new(),
            heat: 0,
            status: IngotStatus::Forging,
            last_activity: Instant::now(),
        });
        self.ingots.last_mut().unwrap()
    }

    /// Refresh the activity clock on every row still forging. Token and
    /// tool events carry no ingot id, so liveness is per-forge, not per-row.
    fn mark_activity(&mut self) {
        let now = Instant::now();
        for row in &mut self.ingots {
            if row.status == IngotStatus::Forging {
                row.last_activity = now;
            }
        }
    }

    fn push_feed(&mut self, color: Color, text: String) {
        self.feed.push_back(FeedLine { color, text });
        while self.feed.len() > FEED_CAP {
            self.feed.pop_front();
        }
    }
}

/// Fold one engine event into the dashboard state. Pure state mutation —
/// no terminal involved, so tests drive it directly.
pub(crate) fn apply_event(state: &mut DashState, event: EngineEvent) {
    match &event {
        EngineEvent::Tokens { usage } => {
            state.totals.add(usage);
            state.turn_tokens += usage.total_tokens;
            state.mark_activity();
        }
        EngineEvent::TurnStart { turn } => {
            state.turn = *turn;
            state.turn_started = Some(Instant::now());
            state.turn_tokens = 0;
        }
        EngineEvent::Finish { .. } | EngineEvent::Error { .. } => {
            state.turn_started = None;
        }
        EngineEvent::ToolResult { .. } => state.mark_activity(),
        EngineEvent::IngotStart { id, work } => {
            let row = state.row_mut(id);
            row.work = preview(work, 60);
            row.heat = 0;
            row.status = IngotStatus::Forging;
            row.last_activity = Instant::now();
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
    state.push_feed(color, text);
}

/// One narrator-style feed line per event (mirrors `StderrNarrator`).
fn feed_entry(event: &EngineEvent) -> (Color, String) {
    match event {
        EngineEvent::TurnStart { turn } => (palette(tui::HOT), format!("⚒ turn {turn}")),
        EngineEvent::ModelCall { model } => (palette(tui::COLD), format!("⚙ {model}")),
        EngineEvent::ModelRouted { routed, .. } => {
            (palette(tui::COLD), format!("⚙ routed to {routed}"))
        }
        EngineEvent::ToolCallStart { name, preview: p } => {
            (palette(tui::BRIGHT), format!("→ {name}: {}", preview(p, 80)))
        }
        EngineEvent::ToolResult { name, ok: true, .. } => {
            (palette(tui::PURE), format!("✓ {name} ok"))
        }
        EngineEvent::ToolResult { name, ok: false, preview: p } => {
            (palette(tui::WARM), format!("✗ {name}: {}", preview(p, 80)))
        }
        EngineEvent::Tokens { usage } => {
            let msg = match usage.cost {
                Some(cost) => format!("◦ {} tok (${cost:.4})", usage.total_tokens),
                None => format!("◦ {} tok", usage.total_tokens),
            };
            (palette(tui::COLD), msg)
        }
        EngineEvent::Steer { text } => {
            (palette(tui::BRIGHT), format!("↪ steer: {}", preview(text, 80)))
        }
        EngineEvent::Finish { summary } => (palette(tui::PURE), format!("■ {}", preview(summary, 120))),
        EngineEvent::Error { message } => (palette(tui::WARM), format!("✗ {}", preview(message, 120))),
        EngineEvent::Narrate { text } => (palette(tui::COLD), format!("◈ {}", preview(text, 110))),
        EngineEvent::Warning { message } => {
            (palette(tui::BRIGHT), format!("⚠ {}", preview(message, 110)))
        }
        EngineEvent::IngotStart { id, work } => {
            (palette(tui::HOT), format!("🧱 [{id}] {}", preview(work, 60)))
        }
        EngineEvent::HeatTick { id, heat } => (palette(tui::WARM), format!("🔥 [{id}] heat {heat}")),
        EngineEvent::IngotDone { id, ok: true } => (palette(tui::PURE), format!("✅ [{id}] forged")),
        EngineEvent::IngotDone { id, ok: false } => (palette(tui::WARM), format!("❌ [{id}] cracked")),
        EngineEvent::DuelRound { id, round } => {
            (palette(tui::BRIGHT), format!("⚔ [{id}] duel round {round}"))
        }
        EngineEvent::DuelVerdict { id, winner, margin } => {
            (palette(tui::PURE), format!("⚖ [{id}] cast {winner} wins by {margin}"))
        }
        EngineEvent::ApiRetry { attempt, status, remaining_secs } => (
            palette(tui::BRIGHT),
            format!("⟳ api {status} — retry {attempt} in {remaining_secs}s"),
        ),
    }
}

/// The bottom bar's live spinner status: `⚒ Forging… (12s · 4.1k tok ·
/// 38 tok/s · esc to interrupt)`. `Some` only while a turn is running and
/// at least one ingot is still forging — otherwise the bar shows plain
/// totals.
pub(crate) fn forge_status(state: &DashState, now: Instant) -> Option<String> {
    let started = state.turn_started?;
    if !state.ingots.iter().any(|r| r.status == IngotStatus::Forging) {
        return None;
    }
    Some(crate::progress::spinner_status(
        tui::forge_verb(state.turn),
        now.saturating_duration_since(started),
        state.turn_tokens,
    ))
}

/// True when any forging row has crossed the stall threshold.
pub(crate) fn has_stalled(state: &DashState, now: Instant) -> bool {
    state.ingots.iter().any(|row| {
        row.status == IngotStatus::Forging
            && now.saturating_duration_since(row.last_activity) >= STALL_WARN
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    Stay,
    /// Leave the dashboard; the forge continues headless.
    Detach,
    /// CancelFlag set; leave the dashboard.
    Cancel,
}

/// Fold one key press into the state. Pure except for the steer queue
/// push and the cancel-flag store.
pub(crate) fn handle_key(
    state: &mut DashState,
    key: KeyEvent,
    steer: &SteerQueue,
    cancel: &CancelFlag,
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
        cancel.store(true, Ordering::SeqCst);
        return KeyOutcome::Cancel;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') if state.input.is_empty() => KeyOutcome::Detach,
        KeyCode::Esc => {
            state.input.clear();
            KeyOutcome::Stay
        }
        KeyCode::Enter => {
            if !state.input.is_empty() {
                let text = std::mem::take(&mut state.input);
                if let Ok(mut q) = steer.lock() {
                    q.push(text);
                }
                state.flash_until = Some(Instant::now() + FLASH);
            }
            KeyOutcome::Stay
        }
        KeyCode::Backspace => {
            state.input.pop();
            KeyOutcome::Stay
        }
        KeyCode::Char(c) => {
            state.input.push(c);
            KeyOutcome::Stay
        }
        _ => KeyOutcome::Stay,
    }
}

// ---------------------------------------------------------------- rendering

pub(crate) fn draw(f: &mut Frame, state: &DashState) {
    let [main, bottom] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(f.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(main);
    draw_crucible(f, left, state);
    draw_feed(f, right, state);
    draw_bottom(f, bottom, state);
}

fn ingot_line(row: &IngotRow, now: Instant) -> Line<'_> {
    let (glyph, mut word, mut color) = match &row.status {
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
    if row.status == IngotStatus::Forging {
        let silent = now.saturating_duration_since(row.last_activity);
        if silent >= STALL_WARN {
            word.push_str(&format!(" (stalled {}s)", silent.as_secs()));
            color = if silent >= STALL_DEAD {
                palette(tui::WARM)
            } else {
                palette(tui::BRIGHT)
            };
        }
    }
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(format!("[{}] ", row.id), Style::default().fg(palette(tui::PURE))),
        Span::styled(word, Style::default().fg(color)),
    ])
}

fn draw_crucible(f: &mut Frame, area: Rect, state: &DashState) {
    let visible = area.height.saturating_sub(2) as usize;
    let skip = state.ingots.len().saturating_sub(visible);
    let now = Instant::now();
    let lines: Vec<Line> =
        state.ingots.iter().skip(skip).map(|row| ingot_line(row, now)).collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" crucible ")
        .border_style(Style::default().fg(palette(tui::COLD)));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_feed(f: &mut Frame, area: Rect, state: &DashState) {
    // Auto-scroll: always show the newest lines that fit.
    let visible = area.height.saturating_sub(2) as usize;
    let skip = state.feed.len().saturating_sub(visible);
    let lines: Vec<Line> = state
        .feed
        .iter()
        .skip(skip)
        .map(|l| Line::from(Span::styled(format!("  {}", l.text), Style::default().fg(l.color))))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" forge feed ")
        .border_style(Style::default().fg(palette(tui::COLD)));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_bottom(f: &mut Frame, area: Rect, state: &DashState) {
    let mut totals = match state.totals.cost {
        Some(cost) => format!("  Σ {} tok · ${cost:.4}", state.totals.total_tokens),
        None => format!("  Σ {} tok", state.totals.total_tokens),
    };
    let mut totals_color = palette(tui::PURE);
    if let Some(status) = forge_status(state, Instant::now()) {
        totals = format!("  {status}");
        totals_color = palette(tui::HOT);
    }
    let mut input_spans = vec![
        Span::styled("  > ", Style::default().fg(palette(tui::HOT))),
        Span::styled(state.input.clone(), Style::default().fg(palette(tui::PURE))),
        Span::styled("▏", Style::default().fg(palette(tui::BRIGHT))),
    ];
    if state.flash_until.is_some_and(|t| Instant::now() < t) {
        input_spans.push(Span::styled(
            "  steer queued",
            Style::default().fg(palette(tui::BRIGHT)),
        ));
    }
    let lines = vec![
        Line::from(Span::styled(totals, Style::default().fg(totals_color))),
        Line::from(input_spans),
        Line::from(Span::styled(format!("  {HINT}"), Style::default().fg(palette(tui::COLD)))),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

// ---------------------------------------------------------------- terminal

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(io::stderr(), LeaveAlternateScreen, crossterm::cursor::Show);
}

/// Panic hook that leaves the alternate screen before the default hook
/// prints, so the backtrace lands on a sane terminal.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
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
) -> io::Result<()> {
    tui::set_quiet(true);
    install_panic_hook();

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

    let result = event_loop(&mut terminal, &mut rx, &mut keys, &steer, &cancel).await;

    stop.store(true, Ordering::Relaxed);
    restore_terminal();
    tui::set_quiet(false);
    eprintln!("  dashboard detached, forge continues");
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    rx: &mut UnboundedReceiver<EngineEvent>,
    keys: &mut UnboundedReceiver<Event>,
    steer: &SteerQueue,
    cancel: &CancelFlag,
) -> io::Result<()> {
    let mut state = DashState::default();
    let mut interval = tokio::time::interval(FRAME);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut engine_done = false;

    loop {
        tokio::select! {
            ev = rx.recv(), if !engine_done => {
                match ev {
                    Some(ev) => {
                        apply_event(&mut state, ev);
                        // Coalesce bursts into one redraw.
                        while let Ok(ev) = rx.try_recv() {
                            apply_event(&mut state, ev);
                        }
                        dirty = true;
                    }
                    None => {
                        engine_done = true;
                        state.push_feed(
                            palette(tui::BRIGHT),
                            "■ forge finished — press q/Esc to exit".into(),
                        );
                        dirty = true;
                    }
                }
            }
            key = keys.recv() => {
                match key {
                    Some(Event::Key(k)) => match handle_key(&mut state, k, steer, cancel) {
                        KeyOutcome::Stay => dirty = true,
                        KeyOutcome::Detach | KeyOutcome::Cancel => return Ok(()),
                    },
                    Some(Event::Resize(..)) => dirty = true,
                    Some(_) => {}
                    None => return Ok(()), // input thread died; leave cleanly
                }
            }
            _ = interval.tick() => {
                if state.flash_until.is_some_and(|t| Instant::now() >= t) {
                    state.flash_until = None;
                    dirty = true;
                }
                // Stalled rows change appearance with no event arriving:
                // keep the "(stalled Ns)" counter ticking on screen.
                if has_stalled(&state, Instant::now()) {
                    dirty = true;
                }
                // Same for the bottom-bar spinner status: its elapsed
                // seconds tick with no event arriving.
                if forge_status(&state, Instant::now()).is_some() {
                    dirty = true;
                }
                if dirty {
                    terminal.draw(|f| draw(f, &state))?;
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
            EngineEvent::ToolResult { name: "bash".into(), ok: true, preview: "ok".into() },
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
        assert_eq!(state.feed.len(), 6);
        assert!(state.feed.back().unwrap().text.contains("[i1] forged"));
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
            EngineEvent::ToolResult { name: "bash".into(), ok: false, preview: "x".into() },
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
        let fresh = ingot_line(row, base);
        assert!(!text_of(&fresh).contains("stalled"));

        // 20s of silence: yellow, "(stalled 20s)".
        let warn = ingot_line(row, base + Duration::from_secs(20));
        assert!(text_of(&warn).contains("(stalled 20s)"));
        assert_eq!(warn.spans[0].style.fg, Some(palette(tui::BRIGHT)));

        // 90s of silence: red.
        let dead = ingot_line(row, base + Duration::from_secs(90));
        assert!(text_of(&dead).contains("(stalled 90s)"));
        assert_eq!(dead.spans[0].style.fg, Some(palette(tui::WARM)));
    }

    #[test]
    fn stall_tint_only_applies_to_forging_rows() {
        let mut state = DashState::default();
        apply_event(&mut state, EngineEvent::IngotStart { id: "i1".into(), work: "w".into() });
        apply_event(&mut state, EngineEvent::IngotDone { id: "i1".into(), ok: true });
        let row = &state.ingots[0];
        let line = ingot_line(row, row.last_activity + Duration::from_secs(90));
        let text: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert!(!text.contains("stalled"), "forged row must never show a stall");
        assert!(text.contains("forged"));
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
        assert_eq!(line, "⚒ Forging… (12s · 4.1k tok · 342 tok/s · esc to interrupt)");

        // A new turn re-zeros the token fold and rotates the verb.
        apply_event(&mut state, EngineEvent::TurnStart { turn: 1 });
        assert_eq!(state.turn_tokens, 0);
        let t1 = state.turn_started.unwrap();
        let line = forge_status(&state, t1 + Duration::from_secs(2)).unwrap();
        assert!(line.starts_with("⚒ Smelting… (2s · 0 tok"), "{line}");
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
        assert!(content.contains("⚒ Forging…"), "spinner status must render");
        assert!(content.contains("esc to interrupt"));
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
        assert!(state.flash_until.is_some());

        // Enter with an empty buffer queues nothing.
        handle_key(&mut state, press(KeyCode::Enter), &steer, &cancel);
        assert_eq!(steer.lock().unwrap().len(), 1);
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
    fn ctrl_c_sets_cancel_flag_and_detaches() {
        let mut state = DashState::default();
        let (steer, cancel) = queue();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut state, key, &steer, &cancel), KeyOutcome::Cancel);
        assert!(cancel.load(Ordering::SeqCst));
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
        state.flash_until = Some(Instant::now() + FLASH);
        terminal.draw(|f| draw(f, &state)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("[i1]"));
        assert!(content.contains("forging"));
        assert!(content.contains("42 tok"));
        assert!(content.contains("steer me"));
        assert!(content.contains("steer queued"));
        // Hint line clips at 80 cols; assert on its head.
        assert!(content.contains("steer the smith"));
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
