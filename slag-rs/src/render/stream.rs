//! stream — the stream view: one writer for the whole run, live or replayed.
//!
//! Two layers share the terminal. The ledger scrolls: the echo's skeleton
//! (`render::echo`, stdout) interleaved with the narrator's tool cards
//! (`engine::events::RenderState`, stderr). The gauge stays put: one
//! footer row pinned to the bottom of the terminal, saying what is running
//! right now and what it has cost so far.
//!
//! The footer is pinned with a scroll region (DECSTBM, the way apt draws
//! its progress row): everything above it scrolls, the last row does not.
//! Nothing the forge prints can land on it, so it never needs the `\r`
//! rewrites that used to eat a half-printed heat line. Off a terminal
//! there is no footer and no escape byte at all: CI logs stay plain lines.
//!
//! `Stream` is the session's viewer in a live headless run and the player
//! in `slag replay`, so a replay draws what the run drew.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Instant;

use crate::engine::events::{RenderOp, RenderState};
use crate::render::echo::{Echo, Phase};
use crate::render::tape::{Cue, Viewer};
use crate::tui::{self, dim, paint, HOT, PURE};

/// Where one ledger line goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// The forge's skeleton: phases, ingots, heats, the assay.
    Std(String),
    /// The smiths' tool cards and the run's warnings.
    Err(String),
}

#[cfg(test)]
impl Out {
    pub fn text(&self) -> &str {
        match self {
            Out::Std(s) | Out::Err(s) => s,
        }
    }
}

pub struct Stream {
    echo: Echo,
    narr: RenderState,
    /// Whether a footer may be drawn (a real terminal on both streams).
    tty: bool,
    width: usize,
    /// Width pinned by the caller (`--size`); `None` follows the terminal.
    pinned: Option<usize>,
    spin: usize,
    footer: Option<String>,
    /// Hold footer redraws until the next tick. A replay jumping straight
    /// to a moment would otherwise redraw it once per cue on the way.
    defer_footer: bool,
}

impl Stream {
    /// The live view: a footer when both streams are a terminal.
    pub fn live() -> Self {
        let tty = std::io::stdout().is_terminal() && std::io::stderr().is_terminal();
        Self::new(tty, None)
    }

    pub fn new(tty: bool, width: Option<usize>) -> Self {
        let w = width.map(|w| w.saturating_sub(1).max(20)).unwrap_or_else(tui::width);
        Self {
            echo: Echo::new(w),
            narr: RenderState::new(false).without_lifecycle().with_footer(tty).with_live_width(w),
            tty,
            width: w,
            pinned: width,
            spin: 0,
            footer: None,
            defer_footer: false,
        }
    }

    /// Print a run-stopping provider error itself (a replay; live runs
    /// leave that line to `main`).
    pub fn owning_fatal(mut self, on: bool) -> Self {
        self.echo = std::mem::replace(&mut self.echo, Echo::new(self.width)).owning_fatal(on);
        self
    }

    /// Draw the footer only on ticks, not on every cue.
    pub fn defer_footer(mut self, on: bool) -> Self {
        self.defer_footer = on;
        self
    }

    /// Follow a resized terminal: every line built from now on fits it.
    fn refresh_width(&mut self) {
        if self.pinned.is_some() {
            return;
        }
        let w = tui::width();
        if w != self.width {
            self.width = w;
            self.echo.set_width(w);
            self.narr.set_width(w);
        }
    }

    /// The ledger lines `cue` adds, in order. Pure: nothing is written.
    pub fn lines(&mut self, cue: &Cue, at: Instant) -> Vec<Out> {
        let mut out = Vec::new();
        // Cards a streak is holding print before the skeleton line that
        // follows them, or a heat's verdict would land above its own work.
        let skeleton = !matches!(cue, Cue::Event(e) if !is_lifecycle(e));
        if skeleton {
            out.extend(prints(self.narr.flush()).into_iter().map(Out::Err));
        }
        if let Cue::Event(e) = cue {
            // A survey or casting pass ends on the document itself; the
            // echo shows its excerpt or its table, so the raw text would
            // only print the blueprint (or the S-expressions) twice. The
            // warden's verdict reaches the stream as its own event.
            let document_pass = matches!(e, crate::engine::EngineEvent::Finish { .. })
                && matches!(self.echo.phase(), Phase::Survey | Phase::Plan | Phase::Warden);
            if !document_pass {
                out.extend(prints(self.narr.feed_at(e, at)).into_iter().map(Out::Err));
            }
        }
        out.extend(self.echo.cue(cue, at).into_iter().map(Out::Std));
        out
    }

    /// The closing lines: held cards, then the assay.
    pub fn closing(&mut self, at: Instant) -> Vec<Out> {
        let mut out: Vec<Out> = prints(self.narr.finish()).into_iter().map(Out::Err).collect();
        out.extend(self.echo.finish(at).into_iter().map(Out::Std));
        out
    }

    /// The footer row for `at`, or `None` when nothing is running.
    pub fn footer_text(&self, at: Instant) -> Option<String> {
        let verb = self.echo.phase_verb()?;
        let verb = verb.as_str();
        let mut left = format!(
            " {} {}",
            paint(HOT, tui::spinner_frame(self.spin)),
            paint(PURE, verb)
        );
        let mut used = 3 + verb.chars().count();
        if self.echo.phase() == Phase::Forge {
            let anvils = self.echo.anvils();
            let ctx = match anvils {
                [] => String::new(),
                [one] => match (one.heat, self.echo.max_heat(&one.id)) {
                    (h, Some(max)) if h > 0 => format!("{} · heat {h}/{max}", one.id),
                    _ => one.id.clone(),
                },
                many => many.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(" "),
            };
            if !ctx.is_empty() {
                used += 1 + ctx.chars().count();
                left.push_str(&format!(" {}", paint(PURE, &ctx)));
            }
        }
        let mut right = Vec::new();
        if let Some(start) = self.echo.started() {
            right.push(format!("run {}", tui::clock(at.saturating_duration_since(start))));
        }
        let usage = self.echo.spend();
        if usage.total_tokens > 0 {
            right.push(format!("{} tok", tui::tokens(usage.total_tokens)));
        }
        if usage.total_tokens > 0 || usage.cost.is_some() {
            right.push(crate::engine::pricing::format_cost(usage));
        }
        let mut right = right.join(" · ");
        if self.width >= 100 {
            right.push_str("   ^C stop");
        }
        if let Some((label, took)) = self.narr.activity_at(at) {
            let room = self.width.saturating_sub(used + right.chars().count() + 10);
            if room >= 12 {
                let act = format!("{} {}", tui::clip(&label, room), tui::clock(took));
                left.push_str(&format!("  {}", dim(&act)));
            }
        }
        Some(tui::spread(&left, &dim(&right), self.width))
    }

    /// Feed a cue and write what it printed. The session's viewer path.
    pub fn cue_at(&mut self, cue: &Cue, at: Instant) {
        self.refresh_width();
        let out = self.lines(cue, at);
        let footer = if self.defer_footer {
            self.footer.clone()
        } else {
            self.tty.then(|| self.footer_text(at)).flatten()
        };
        self.write(&out, footer);
    }

    /// Advance the spinner and the clocks on the footer.
    pub fn tick_at(&mut self, at: Instant) {
        if !self.tty {
            return;
        }
        self.spin = self.spin.wrapping_add(1);
        let footer = self.footer_text(at);
        if footer != self.footer {
            self.write(&[], footer);
        }
    }

    pub fn finish_at(&mut self, at: Instant) {
        let out = self.closing(at);
        self.write(&out, None);
        footer_off();
    }

    fn write(&mut self, out: &[Out], footer: Option<String>) {
        let stdout = std::io::stdout();
        let stderr = std::io::stderr();
        let mut o = stdout.lock();
        let mut e = stderr.lock();
        for line in out {
            let _ = match line {
                Out::Std(s) => writeln!(o, "{s}").and_then(|()| o.flush()),
                Out::Err(s) => writeln!(e, "{s}"),
            };
        }
        // The footer row sits outside the scroll region, so printed lines
        // never disturb it: it is redrawn only when its text changes.
        if self.tty && footer != self.footer {
            match &footer {
                Some(text) => footer_draw(&mut e, text),
                None => footer_clear(&mut e),
            }
        }
        let _ = e.flush();
        self.footer = footer;
    }
}

impl Viewer for Stream {
    fn cue(&mut self, cue: &Cue, at: Instant) {
        self.cue_at(cue, at);
    }

    fn tick(&mut self, at: Instant) {
        self.tick_at(at);
    }

    fn finish(&mut self, at: Instant) {
        self.finish_at(at);
    }
}

fn is_lifecycle(e: &crate::engine::EngineEvent) -> bool {
    use crate::engine::EngineEvent as E;
    matches!(
        e,
        E::IngotStart { .. }
            | E::HeatTick { .. }
            | E::IngotDone { .. }
            | E::DuelRound { .. }
            | E::DuelVerdict { .. }
    )
}

/// The printed lines among a narrator's ops. The stream view owns its own
/// footer, so the narrator never emits a live-line rewrite here.
fn prints(ops: Vec<RenderOp>) -> Vec<String> {
    ops.into_iter()
        .filter_map(|op| match op {
            RenderOp::Print(s) => Some(s),
            _ => None,
        })
        .collect()
}

// ─── the pinned footer ──────────────────────────────────────────────────

/// Whether a scroll region is set, and for how many rows.
static FOOTER_ON: AtomicBool = AtomicBool::new(false);
static FOOTER_ROWS: AtomicU16 = AtomicU16::new(0);
static RESTORE_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Reserve the last row and draw `text` on it. The cursor goes back where
/// it was (DECSC/DECRC), so a half-printed line above keeps its column.
fn footer_draw(e: &mut impl Write, text: &str) {
    let rows = crossterm::terminal::size().map(|(_, r)| r).unwrap_or(24);
    if rows < 4 {
        return;
    }
    if !FOOTER_ON.load(Ordering::Relaxed) || FOOTER_ROWS.load(Ordering::Relaxed) != rows {
        if FOOTER_ON.load(Ordering::Relaxed) {
            // Resized: drop the old region before claiming the new last row.
            let _ = write!(e, "\x1b7\x1b[r\x1b8");
        }
        // Make sure a free row exists below the cursor (scrolling if the
        // cursor sits on the last row), then fence it off. The cursor hides
        // while the footer is up: nothing here takes input, and a block
        // blinking under the ledger reads as a prompt.
        let _ = write!(e, "\n\x1b[1A\x1b7\x1b[1;{}r\x1b8\x1b[?25l", rows - 1);
        FOOTER_ON.store(true, Ordering::Relaxed);
        FOOTER_ROWS.store(rows, Ordering::Relaxed);
        if !RESTORE_REGISTERED.swap(true, Ordering::Relaxed) {
            // A Ctrl-C or a panic must not leave the shell fenced in.
            crate::shutdown::register(footer_off);
        }
    }
    let _ = write!(e, "\x1b7\x1b[{rows};1H\x1b[2K{text}\x1b8");
}

fn footer_clear(e: &mut impl Write) {
    if FOOTER_ON.load(Ordering::Relaxed) {
        let rows = FOOTER_ROWS.load(Ordering::Relaxed);
        let _ = write!(e, "\x1b7\x1b[{rows};1H\x1b[2K\x1b8");
    }
}

/// Give the whole screen back: clear the footer row, drop the region.
/// Idempotent, and safe from the shutdown registry.
pub fn footer_off() {
    if !FOOTER_ON.swap(false, Ordering::Relaxed) {
        return;
    }
    let rows = FOOTER_ROWS.load(Ordering::Relaxed);
    let mut e = std::io::stderr().lock();
    let _ = write!(e, "\x1b7\x1b[{rows};1H\x1b[2K\x1b[r\x1b8\x1b[?25h");
    let _ = e.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineEvent;
    use crate::render::tape::Mark;
    use std::time::Duration;

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn a_held_card_prints_before_the_heat_that_follows_it() {
        let mut s = Stream::new(false, Some(100));
        let t0 = Instant::now();
        let mut out = Vec::new();
        let plan = r#"(ingot :id "i1" :status molten :solo nil :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "a")"#;
        out.extend(s.lines(&Cue::Plan(plan.into()), t0));
        out.extend(s.lines(&Cue::Event(EngineEvent::IngotStart { id: "i1".into(), work: "a".into() }), t0));
        out.extend(s.lines(&Cue::Event(EngineEvent::HeatTick { id: "i1".into(), heat: 1 }), t0));
        for cmd in ["ls", "pwd"] {
            out.extend(s.lines(
                &Cue::Event(EngineEvent::ToolCallStart {
                    name: "bash".into(),
                    preview: format!(r#"{{"command": "{cmd}"}}"#),
                }),
                t0,
            ));
            out.extend(s.lines(
                &Cue::Event(EngineEvent::ToolResult {
                    name: "bash".into(),
                    ok: true,
                    preview: "ok".into(),
                    lines: 1,
                    bytes: 2,
                    ms: 5,
                }),
                t0,
            ));
        }
        out.extend(s.lines(&Cue::Event(EngineEvent::HeatTick { id: "i1".into(), heat: 2 }), t0));
        let text: Vec<String> = out.iter().map(|o| strip(o.text())).collect();
        let pwd = text.iter().position(|l| l.contains("run    pwd")).expect(&text.join("\n"));
        let heat = text.iter().position(|l| l.contains("i1 heat 1/5")).expect(&text.join("\n"));
        assert!(pwd < heat, "{}", text.join("\n"));
    }

    #[test]
    fn the_footer_names_the_phase_the_ingot_and_the_spend() {
        let mut s = Stream::new(true, Some(120));
        let t0 = Instant::now();
        let plan = r#"(ingot :id "i2" :status molten :solo nil :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "a")"#;
        s.lines(&Cue::Mark(Mark::Header { text: "FORGE".into() }), t0);
        s.lines(&Cue::Plan(plan.into()), t0);
        s.lines(&Cue::Event(EngineEvent::IngotStart { id: "i2".into(), work: "a".into() }), t0);
        s.lines(&Cue::Event(EngineEvent::HeatTick { id: "i2".into(), heat: 2 }), t0);
        s.lines(
            &Cue::Event(EngineEvent::Tokens {
                usage: crate::engine::Usage { total_tokens: 41_000, cost: Some(0.06), ..Default::default() },
            }),
            t0,
        );
        s.lines(
            &Cue::Event(EngineEvent::ToolCallStart {
                name: "bash".into(),
                preview: r#"{"command": "node --test"}"#.into(),
            }),
            t0 + Duration::from_secs(100),
        );
        let f = strip(&s.footer_text(t0 + Duration::from_secs(112)).unwrap());
        assert!(f.contains("forging i2 · heat 2/5"), "{f}");
        assert!(f.contains("run node --test 0:12"), "{f}");
        assert!(f.contains("1:52 · 41k tok · $0.0600"), "{f}");
        assert!(tui::visible_len(&f) <= 119, "{}", f.len());
    }

    #[test]
    fn footer_preserves_estimates_and_unknown_tokens() {
        let mut s = Stream::new(true, Some(160));
        let now = Instant::now();
        s.lines(&Cue::Mark(Mark::Header { text: "FORGE".into() }), now);
        s.lines(&Cue::Event(EngineEvent::Tokens {
            usage: crate::engine::Usage {
                prompt_tokens: 1000, completion_tokens: 200, total_tokens: 1200,
                cost: Some(0.0031), estimated: true, ..Default::default()
            },
        }), now);
        assert!(strip(&s.footer_text(now).unwrap()).contains("≈ $0.0031 est."));
        s.lines(&Cue::Event(EngineEvent::Tokens {
            usage: crate::engine::Usage {
                prompt_tokens: 1000, completion_tokens: 200, total_tokens: 1200,
                ..Default::default()
            },
        }), now);
        let footer = strip(&s.footer_text(now).unwrap());
        assert!(footer.contains("spend unknown"), "{footer}");
        assert!(footer.contains("2.0k in / 400 out tokens"), "{footer}");
        assert!(!footer.contains("$0.0031"), "{footer}");
    }

    #[test]
    fn a_flow_this_view_has_no_word_for_still_gets_a_footer() {
        let mut s = Stream::new(true, Some(80));
        let t0 = Instant::now();
        s.lines(&Cue::Mark(Mark::Header { text: "FIX · reproducing the issue".into() }), t0);
        let f = strip(&s.footer_text(t0).unwrap());
        assert!(f.contains(" fix"), "{f}");
    }

    #[test]
    fn no_footer_once_the_run_is_in_its_assay() {
        let mut s = Stream::new(true, Some(80));
        let t0 = Instant::now();
        assert!(s.footer_text(t0).is_none(), "nothing runs before the first phase");
        s.lines(&Cue::Mark(Mark::Header { text: "SURVEYOR · deep analysis".into() }), t0);
        assert!(s.footer_text(t0).is_some());
        s.lines(&Cue::Mark(Mark::Header { text: "ASSAY".into() }), t0);
        assert!(s.footer_text(t0).is_none());
    }
}
