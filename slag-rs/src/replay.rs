//! replay — `slag replay <tape>`: play a recorded run through the live
//! renderers, with no key and no network.
//!
//! The stream view is the same `RenderState` a live run narrates with,
//! plus `render::echo` for the lines the pipeline prints itself. The
//! dashboard is the same `apply_event` and `draw`. Both run on the tape's
//! clock, not the wall clock: every elapsed figure, spinner frame and
//! stall tint comes from the tape, so a replay stopped at the same point
//! draws the same frame every time.
//!
//! `--at` stops on a moment (`retry`, `40%`, `12s`) and holds the frame
//! there until a key or `--hold` runs out; that is the screenshot hook.
//! `--size WxH` draws into an off-screen grid of that size and prints it as
//! text, which is what the tests read.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::error::SlagError;
use crate::render::stream::Stream;
use crate::render::tape::{self, Cue, Tape};

/// A recorded gap longer than this plays as this, before `--speed`. Model
/// calls run for tens of seconds; nobody watching a replay wants to sit
/// through them, and the on-screen clocks still show the real figures.
const MAX_GAP: Duration = Duration::from_millis(1500);

pub struct Opts {
    pub file: PathBuf,
    pub speed: f64,
    pub tui: bool,
    pub at: Option<String>,
    pub hold: Option<f64>,
    pub size: Option<(u16, u16)>,
    pub points: bool,
}

/// `120x36` → `(120, 36)`.
pub fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("size `{s}` should look like 120x36"))?;
    let w: u16 = w.trim().parse().map_err(|_| format!("bad width in `{s}`"))?;
    let h: u16 = h.trim().parse().map_err(|_| format!("bad height in `{s}`"))?;
    if w < 20 || h < 6 {
        return Err(format!("size `{s}` is too small to draw; use at least 20x6"));
    }
    Ok((w, h))
}

pub async fn run(opts: Opts) -> Result<(), SlagError> {
    let tape = tape::read(&opts.file).map_err(fail)?;
    if tape.cues.is_empty() {
        return Err(fail(format!("{} holds no events to replay", opts.file.display())));
    }
    if opts.points {
        print_points(&tape);
        return Ok(());
    }
    let stop = match &opts.at {
        Some(spec) => Some(tape::locate(&tape, spec).map_err(fail)?),
        None => None,
    };
    if opts.tui {
        play_dashboard(&tape, &opts, stop).await
    } else {
        play_stream(&tape, &opts, stop).await;
        Ok(())
    }
}

fn fail(msg: String) -> SlagError {
    SlagError::Other(anyhow::anyhow!(msg))
}

/// `slag replay --points`: every named moment this tape has, and where.
fn print_points(tape: &Tape) {
    for (name, about) in tape::POINTS {
        match tape::locate(tape, name) {
            Ok(t) => println!("  {name:<8} {:>7.1}s  {about}", t as f64 / 1000.0),
            Err(_) => println!("  {name:<8} {:>8}  {about}", "—"),
        }
    }
}

/// Real time to wait before a cue `gap` ms after the last one.
fn pace(gap: u64, speed: f64) -> Duration {
    let gap = Duration::from_millis(gap).min(MAX_GAP);
    gap.div_f64(speed.max(0.01))
}

/// How long to hold the last frame: `--hold` when given, otherwise until a
/// key when stopped at a point, otherwise not at all.
fn hold_for(opts: &Opts, stopped: bool) -> Option<Duration> {
    match opts.hold {
        Some(secs) => Some(Duration::from_secs_f64(secs.max(0.0))),
        None if stopped => Some(Duration::from_secs(24 * 3600)),
        None => None,
    }
}

async fn play_stream(tape: &Tape, opts: &Opts, stop: Option<u64>) {
    let tty = std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
        && opts.size.is_none();
    if let Some((w, _)) = opts.size {
        crate::tui::set_width(w as usize);
    }
    let mut view = Stream::new(tty, opts.size.map(|(w, _)| w as usize));
    let epoch = Instant::now();
    let instant = stop.is_some();
    let mut last_t = 0u64;
    for timed in &tape.cues {
        if stop.is_some_and(|s| timed.t > s) {
            break;
        }
        if !instant {
            // Sleep in footer-sized steps, so its clocks move with the play.
            let wait = pace(timed.t - last_t, opts.speed);
            let resume = Instant::now() + wait;
            loop {
                let left = resume.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                tokio::time::sleep(left.min(crate::render::tape::TICK)).await;
                let played = wait.saturating_sub(resume.saturating_duration_since(Instant::now()));
                let virt = last_t + played.mul_f64(opts.speed.max(0.01)).as_millis() as u64;
                view.tick_at(epoch + Duration::from_millis(virt.min(timed.t)));
            }
        }
        last_t = timed.t;
        view.cue_at(&timed.cue, epoch + Duration::from_millis(timed.t));
    }
    let stopped_early = stop.is_some_and(|s| s < tape.duration());
    if stopped_early {
        // Draw the footer as it stood at the stop, then hold the frame.
        view.tick_at(epoch + Duration::from_millis(stop.unwrap_or(0)));
    } else {
        view.finish_at(epoch + Duration::from_millis(tape.duration()));
    }
    if let Some(hold) = hold_for(opts, stopped_early) {
        tokio::time::sleep(hold).await;
    }
    crate::render::stream::footer_off();
}

async fn play_dashboard(tape: &Tape, opts: &Opts, stop: Option<u64>) -> Result<(), SlagError> {
    let epoch = Instant::now();
    let commission = tape.cues.iter().find_map(|c| match &c.cue {
        Cue::Doc { name, text } if name == crate::config::ORE_FILE => Some(text.as_str()),
        _ => None,
    });
    let mut state = crate::dashboard::replay_state(tape.first_plan(), commission, epoch);
    let end = stop.unwrap_or(tape.duration());

    // Off-screen: fold every cue up to the stop, draw once, print rows.
    if opts.size.is_some() || !std::io::stderr().is_terminal() {
        let (w, h) = opts.size.unwrap_or((120, 36));
        fold_until(&mut state, tape, end, epoch);
        if end >= tape.duration() {
            crate::dashboard::mark_finished(&mut state);
        }
        for row in crate::dashboard::render_text(&state, w, h) {
            println!("{row}");
        }
        return Ok(());
    }

    let mut terminal = crate::dashboard::replay_terminal().map_err(SlagError::Io)?;
    let result = drive_dashboard(&mut terminal, &mut state, tape, opts, stop, epoch).await;
    crate::dashboard::replay_restore();
    result
}

fn fold_until(state: &mut crate::dashboard::DashState, tape: &Tape, end: u64, epoch: Instant) {
    for timed in tape.cues.iter().take_while(|c| c.t <= end) {
        state.clock = Some(epoch + Duration::from_millis(timed.t));
        if let Cue::Event(e) = &timed.cue {
            crate::dashboard::apply_event(state, e.clone());
        }
    }
    state.clock = Some(epoch + Duration::from_millis(end));
}

async fn drive_dashboard(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stderr>>,
    state: &mut crate::dashboard::DashState,
    tape: &Tape,
    opts: &Opts,
    stop: Option<u64>,
    epoch: Instant,
) -> Result<(), SlagError> {
    let end = stop.unwrap_or(tape.duration());
    if stop.is_some() {
        fold_until(state, tape, end, epoch);
    } else {
        let frame = Duration::from_millis(33);
        let mut last_t = 0u64;
        let mut drawn_at = Instant::now();
        for timed in tape.cues.iter() {
            let wait = pace(timed.t - last_t, opts.speed);
            // Keep drawing through a long wait so the clocks on screen move.
            let resume = Instant::now() + wait;
            while Instant::now() < resume {
                if quit_pressed(Duration::from_millis(1))? {
                    return Ok(());
                }
                let left = resume.saturating_duration_since(Instant::now());
                tokio::time::sleep(left.min(frame)).await;
                let played = wait.saturating_sub(resume.saturating_duration_since(Instant::now()));
                let virt = last_t + (played.mul_f64(opts.speed.max(0.01))).as_millis() as u64;
                state.clock = Some(epoch + Duration::from_millis(virt.min(timed.t)));
                crate::dashboard::replay_draw(terminal, state).map_err(SlagError::Io)?;
                drawn_at = Instant::now();
            }
            last_t = timed.t;
            state.clock = Some(epoch + Duration::from_millis(timed.t));
            if let Cue::Event(e) = &timed.cue {
                crate::dashboard::apply_event(state, e.clone());
            }
            if drawn_at.elapsed() >= frame {
                crate::dashboard::replay_draw(terminal, state).map_err(SlagError::Io)?;
                drawn_at = Instant::now();
            }
        }
        crate::dashboard::mark_finished(state);
    }
    if end >= tape.duration() && stop.is_some() {
        crate::dashboard::mark_finished(state);
    }
    crate::dashboard::replay_draw(terminal, state).map_err(SlagError::Io)?;

    // Hold the last frame. A finished replay waits for a key like a
    // finished live forge does; a stopped one holds for the screenshot.
    let hold = hold_for(opts, true).unwrap_or(Duration::ZERO);
    let until = Instant::now() + hold;
    while Instant::now() < until {
        if quit_pressed(Duration::from_millis(100))? {
            break;
        }
    }
    Ok(())
}

/// q, Esc or Ctrl-C within `wait`. Raw mode swallows the shell's Ctrl-C,
/// so the replay has to hear it as a key.
fn quit_pressed(wait: Duration) -> Result<bool, SlagError> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    if !crossterm::event::poll(wait).map_err(SlagError::Io)? {
        return Ok(false);
    }
    match crossterm::event::read().map_err(SlagError::Io)? {
        Event::Key(k) if k.kind == KeyEventKind::Press => Ok(matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
            || (k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')))),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_reads_wxh_and_refuses_nonsense() {
        assert_eq!(parse_size("120x36").unwrap(), (120, 36));
        assert_eq!(parse_size("80X24").unwrap(), (80, 24));
        assert!(parse_size("80").is_err());
        assert!(parse_size("axb").is_err());
        assert!(parse_size("10x3").is_err());
    }

    #[test]
    fn long_gaps_play_short_and_speed_scales_them() {
        assert_eq!(pace(200, 1.0), Duration::from_millis(200));
        assert_eq!(pace(60_000, 1.0), MAX_GAP);
        assert_eq!(pace(1000, 2.0), Duration::from_millis(500));
        // A zero or negative speed must not divide by zero.
        assert!(pace(1000, 0.0) > Duration::ZERO);
    }
}
