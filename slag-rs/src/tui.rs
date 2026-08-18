use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor, ResetColor};
use indicatif::{ProgressBar, ProgressStyle};

use crate::crucible::CrucibleCounts;

/// Stream-mode silencer: the Ratatui dashboard owns the screen while set.
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(q: bool) {
    QUIET.store(q, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

// Palette (cold ore → hot metal → pure steel).
// Same five hexes the slag.dev site uses (website/src/main.css `--slag-*`),
// so terminal and web read as one product. Terminals without truecolor get
// the nearest ANSI value via `downgrade`.
pub const COLD: Color = Color::Rgb { r: 0x6b, g: 0x73, b: 0x85 };
pub const WARM: Color = Color::Rgb { r: 0xe0, g: 0x6c, b: 0x75 };
pub const HOT: Color = Color::Rgb { r: 0xff, g: 0x99, b: 0x40 };
pub const BRIGHT: Color = Color::Rgb { r: 0xff, g: 0xd8, b: 0x66 };
pub const PURE: Color = Color::Rgb { r: 0xff, g: 0xff, b: 0xff };

/// True when the terminal advertises 24-bit color. Checked once: the
/// answer cannot change mid-run, and every printed line asks.
pub fn truecolor() -> bool {
    static YES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *YES.get_or_init(|| {
        std::env::var("COLORTERM")
            .map(|v| {
                let v = v.to_lowercase();
                v.contains("truecolor") || v.contains("24bit")
            })
            .unwrap_or(false)
    })
}

/// Whether to emit color at all. Redirected output and NO_COLOR both mean
/// no: escape bytes in a log file or a pasted bug report help nobody, and
/// they break `grep` on a colored word.
pub fn colored() -> bool {
    static YES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *YES.get_or_init(|| {
        colored_from(
            std::env::var_os("NO_COLOR").is_some(),
            std::io::stdout().is_terminal(),
            std::io::stderr().is_terminal(),
        )
    })
}

/// Pure color policy: painted text lands on BOTH streams (stdout status
/// lines, stderr narrator), so either stream being redirected turns color
/// off everywhere — `slag forge 2> err.log` must not fill err.log with
/// escape bytes just because stdout is still a terminal.
fn colored_from(no_color: bool, stdout_tty: bool, stderr_tty: bool) -> bool {
    !no_color && stdout_tty && stderr_tty
}

/// Map a palette color to what the terminal can actually render. Sending
/// 24-bit escapes to a 256-color terminal paints unreadable approximations.
pub fn downgrade(color: Color) -> Color {
    if truecolor() {
        return color;
    }
    match color {
        COLD => Color::DarkGrey,
        WARM => Color::Red,
        HOT => Color::AnsiValue(208),
        BRIGHT => Color::AnsiValue(220),
        PURE => Color::White,
        other => other,
    }
}

pub fn hr() {
    if is_quiet() {
        return;
    }
    println!(
        "{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        fg(COLD),
        reset()
    );
}

pub fn header(title: &str) {
    if is_quiet() {
        return;
    }
    println!();
    hr();
    println!(
        "{}{}  \u{2692} {}{}{}",
        bold(),
        fg(PURE),
        title,
        reset(),
        unbold(),
    );
    hr();
}

pub fn status_line(icon: &str, color: Color, msg: &str) {
    if is_quiet() {
        return;
    }
    println!("  {}{}{} {}", fg(color), icon, reset(), msg);
}

pub fn show_banner() {
    if is_quiet() {
        return;
    }
    println!();
    print!("  {}░░░", fg(COLD));
    print!("{}▒", fg(WARM));
    print!("{}▒", fg(HOT));
    print!("{}▓", fg(BRIGHT));
    print!("{}█", fg(PURE));
    print!(
        "  {}{}SLAG{}",
        bold(),
        fg(PURE),
        unbold(),
    );
    print!("  {}█", fg(PURE));
    print!("{}▓", fg(BRIGHT));
    print!("{}▒", fg(HOT));
    print!("{}▒", fg(WARM));
    println!("{}░░░{}", fg(COLD), reset());

    println!("  {}cold      hot       pure{}", fg(COLD), reset());
    println!(
        "  {}survey · design · forge · temper{}",
        fg(COLD),
        reset()
    );
}

/// First-run key prompt. Two lines of why, one line to act on, then the
/// cursor. Everything slag needs to run fits on this screen.
pub fn key_intro() {
    show_banner();
    println!();
    println!(
        "  {}slag forges through OpenRouter. One key, every model.{}",
        fg(PURE),
        reset()
    );
    println!(
        "  {}get one at{} {}https://openrouter.ai/keys{}",
        fg(COLD),
        reset(),
        fg(BRIGHT),
        reset()
    );
    println!();
    print!("  {}key{} {}› {}", fg(HOT), reset(), fg(COLD), reset());
    flush();
}

/// Saved a key slag could not reach OpenRouter to check. Say so, so a
/// later 401 does not read like a mystery.
pub fn key_unverified(why: &str) {
    println!(
        "  {}▒{} could not reach OpenRouter ({}) — saving unverified",
        fg(WARM),
        reset(),
        why
    );
}

/// Confirm where the key landed, so the user can find or delete it later.
pub fn key_saved(path: &std::path::Path) {
    println!(
        "  {}█{} saved to {}{}{}",
        fg(PURE),
        reset(),
        fg(COLD),
        path.display(),
        reset()
    );
    println!();
}

/// `slag key` with no argument: state of the one setting slag has.
/// Each model row may carry a note explaining when that role is inactive.
pub fn key_panel(source: Option<(&str, String)>, models: &[(&str, &str, Option<&str>)]) {
    show_banner();
    println!();
    match source {
        Some((from, masked)) => {
            println!(
                "  {}key{}    {}{}{}  {}from {}{}",
                fg(COLD),
                reset(),
                fg(PURE),
                masked,
                reset(),
                fg(COLD),
                from,
                reset()
            );
        }
        None => {
            println!(
                "  {}key{}    {}none{}  {}run `slag key` or set OPENROUTER_API_KEY{}",
                fg(COLD),
                reset(),
                fg(WARM),
                reset(),
                fg(COLD),
                reset()
            );
        }
    }
    for (role, model, note) in models {
        println!(
            "  {}{:<6}{} {}{}{}{}",
            fg(COLD),
            role,
            reset(),
            fg(BRIGHT),
            model,
            reset(),
            match note {
                Some(note) => format!("  {}{note}{}", fg(COLD), reset()),
                None => String::new(),
            }
        );
    }
    println!();
}

/// A file key that the environment overrides is a key that does nothing.
/// Say so at the moment of saving, not three 401s later.
pub fn key_shadowed() {
    println!(
        "  {}▒{} OPENROUTER_API_KEY is set and wins over the saved key — \
         unset it to use this one",
        fg(WARM),
        reset()
    );
}

pub fn ingot_status_line(counts: &CrucibleCounts) {
    if is_quiet() {
        return;
    }
    print!("[ ✅{} done | 🔥{} forging | 🧱{} queued", counts.forged, counts.molten, counts.ore);
    if counts.cracked > 0 {
        print!(" | ❌{} failed", counts.cracked);
    }
    print!(" ]");
}

pub fn temper_bar(counts: &CrucibleCounts) {
    if is_quiet() {
        return;
    }
    let total = counts.total.max(1);
    let pct = counts.forged * 100 / total;
    let filled = counts.forged * 20 / total;
    let empty = 20 - filled;

    print!("  {}[{}", fg(COLD), reset());
    for i in 0..filled {
        if i < filled / 3 {
            print!("{}▒{}", fg(WARM), reset());
        } else if i < filled * 2 / 3 {
            print!("{}▓{}", fg(HOT), reset());
        } else {
            print!("{}█{}", fg(BRIGHT), reset());
        }
    }
    for _ in 0..empty {
        print!("{}░{}", fg(COLD), reset());
    }
    println!(
        "{}]{} {}{}%{}",
        fg(COLD),
        reset(),
        fg(PURE),
        pct,
        reset()
    );
}

/// Create a spinner for long operations
pub fn spinner(msg: &str) -> ProgressBar {
    if is_quiet() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("◐◓◑◒ ")
            .template(&format!("   {{spinner}} {msg}"))
            .unwrap(),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(150));
    pb
}

/// Create a spark-style spinner
pub fn spark_spinner(msg: &str) -> ProgressBar {
    if is_quiet() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["ite", "·te", "··e", "···", "i··", "it·"])
            .template(&format!("   {{spinner}} {msg}"))
            .unwrap(),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(150));
    pb
}

/// Spinner frames for the stream-mode live line (see `engine::events`).
pub const SPINNER_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// Frame for tick `i`; wraps forever, so callers just count up.
pub fn spinner_frame(i: usize) -> &'static str {
    SPINNER_FRAMES[i % SPINNER_FRAMES.len()]
}

/// Metallurgical verbs for the live spinner status line. One per turn,
/// picked by turn number, so a long forge reads like shop work instead of
/// repeating "Forging…" forever.
pub const FORGE_VERBS: [&str; 12] = [
    "Forging",
    "Smelting",
    "Hammering",
    "Tempering",
    "Annealing",
    "Quenching",
    "Casting",
    "Striking",
    "Alloying",
    "Riveting",
    "Sintering",
    "Burnishing",
];

/// Verb for turn `i`; wraps forever like `spinner_frame`.
pub fn forge_verb(i: usize) -> &'static str {
    FORGE_VERBS[i % FORGE_VERBS.len()]
}

// ─── terminal notifications ─────────────────────────────────────────────
//
// A finished (or failed) forge should ping the user who tabbed away —
// but never the one actively typing in the dashboard. The dashboard
// records every keypress via `mark_user_activity`; `notify` only fires
// after `NOTIFY_IDLE` of silence. Headless runs never record activity,
// so they always notify.

/// User must be hands-off this long before a notification fires.
pub const NOTIFY_IDLE: Duration = Duration::from_secs(6);

/// Last dashboard keypress. `None` = no interactive session ever touched
/// this process (headless forge), which counts as idle.
static LAST_KEYPRESS: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Record a user keypress (dashboard input loop).
pub fn mark_user_activity() {
    if let Ok(mut last) = LAST_KEYPRESS.lock() {
        *last = Some(Instant::now());
    }
}

pub(crate) fn last_user_activity() -> Option<Instant> {
    LAST_KEYPRESS.lock().ok().and_then(|l| *l)
}

/// Pure idleness gate: no recorded activity counts as idle.
pub fn idle_enough(last: Option<Instant>, now: Instant, threshold: Duration) -> bool {
    match last {
        None => true,
        Some(t) => now.saturating_duration_since(t) >= threshold,
    }
}

/// Which notification escape this terminal understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyProto {
    /// iTerm2-style `OSC 9 ; message BEL`.
    Osc9,
    /// kitty desktop notifications, `OSC 99`.
    Osc99,
    /// urxvt/WezTerm/ghostty `OSC 777 ; notify ; title ; body`.
    Osc777,
    /// Unknown terminal: the BEL alone still dings and marks the tab.
    BelOnly,
}

/// Detect the notification protocol from `TERM_PROGRAM` (primary) and
/// `TERM` (fallback). Unknown terminals get BEL only — a wrong OSC would
/// print garbage into the scrollback.
pub fn notify_proto(term_program: Option<&str>, term: Option<&str>) -> NotifyProto {
    match term_program.unwrap_or("") {
        "iTerm.app" => return NotifyProto::Osc9,
        "kitty" => return NotifyProto::Osc99,
        "WezTerm" | "ghostty" => return NotifyProto::Osc777,
        "Apple_Terminal" => return NotifyProto::BelOnly,
        _ => {}
    }
    let term = term.unwrap_or("");
    if term.contains("kitty") {
        NotifyProto::Osc99
    } else if term.contains("rxvt") {
        NotifyProto::Osc777
    } else {
        NotifyProto::BelOnly
    }
}

/// Strip control characters (ESC included) so notification text can never
/// smuggle its own escape sequences into the terminal, and `;` for the
/// OSC 777 field separator.
fn notify_clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() || c == ';' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// The bytes to write: BEL first (audible bell + tab marker everywhere),
/// then the richest OSC the terminal understands.
pub fn notify_sequence(proto: NotifyProto, title: &str, body: &str) -> String {
    let title = notify_clean(title);
    let body = notify_clean(body);
    let mut s = String::from("\x07");
    match proto {
        NotifyProto::Osc9 => s.push_str(&format!("\x1b]9;{title}: {body}\x07")),
        NotifyProto::Osc99 => s.push_str(&format!("\x1b]99;;{title}: {body}\x1b\\")),
        NotifyProto::Osc777 => s.push_str(&format!("\x1b]777;notify;{title};{body}\x1b\\")),
        NotifyProto::BelOnly => {}
    }
    s
}

/// Send a terminal notification, gated on user idleness and a real
/// terminal. Silent no-op when the user typed within `NOTIFY_IDLE` (they
/// are watching) or stderr is redirected (escape bytes in a log file).
pub fn notify(title: &str, body: &str) {
    if !idle_enough(last_user_activity(), Instant::now(), NOTIFY_IDLE) {
        return;
    }
    let proto = notify_proto(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    write_osc(&notify_sequence(proto, title, body));
}

// ─── OSC plumbing: progress, title, multiplexer passthrough ─────────────
//
// Terminal chrome (taskbar progress pips, the tab title) speaks OSC.
// Inside tmux/screen a bare OSC stops at the multiplexer, so every
// emission funnels through `write_osc`, which wraps the sequence in a DCS
// passthrough envelope when `$TMUX`/`$STY` says one is in the way.

/// Which terminal multiplexer wraps this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    None,
    Tmux,
    Screen,
}

/// `$TMUX` beats `$STY`: tmux-inside-screen keeps `$STY` around, but the
/// innermost multiplexer is the one that must be tunneled through.
pub fn detect_multiplexer(tmux: Option<&str>, sty: Option<&str>) -> Multiplexer {
    if tmux.is_some_and(|v| !v.is_empty()) {
        Multiplexer::Tmux
    } else if sty.is_some_and(|v| !v.is_empty()) {
        Multiplexer::Screen
    } else {
        Multiplexer::None
    }
}

fn current_multiplexer() -> Multiplexer {
    detect_multiplexer(
        std::env::var("TMUX").ok().as_deref(),
        std::env::var("STY").ok().as_deref(),
    )
}

/// DCS passthrough envelope (Claude Code's `wrapForMultiplexer`): tmux
/// needs every inner ESC doubled inside `ESC Ptmux; … ESC \`; screen takes
/// the sequence raw inside `ESC P … ESC \`.
pub fn wrap_for_multiplexer(seq: &str, mux: Multiplexer) -> String {
    match mux {
        Multiplexer::None => seq.to_string(),
        Multiplexer::Tmux => {
            format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
        }
        Multiplexer::Screen => format!("\x1bP{seq}\x1b\\"),
    }
}

/// The single funnel for every OSC emission. Gates on a real stderr
/// terminal (escape bytes in a redirected log help nobody), wraps for the
/// multiplexer, writes, flushes.
pub fn write_osc(seq: &str) {
    if !std::io::stderr().is_terminal() {
        return;
    }
    let wrapped = wrap_for_multiplexer(seq, current_multiplexer());
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(wrapped.as_bytes());
    let _ = err.flush();
}

/// OSC 9;4 taskbar progress states (ConEmu dialect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscProgress {
    /// Remove the progress pip.
    Clear,
    /// Determinate progress, 0–100.
    Set(u8),
    /// Error state (red pip) — a crack happened.
    Error,
}

pub fn osc_progress_sequence(p: OscProgress) -> String {
    match p {
        OscProgress::Clear => "\x1b]9;4;0;\x1b\\".into(),
        OscProgress::Set(pct) => format!("\x1b]9;4;1;{}\x1b\\", pct.min(100)),
        OscProgress::Error => "\x1b]9;4;2;\x1b\\".into(),
    }
}

/// Terminals that render OSC 9;4 progress. Claude Code's allowlist minus
/// Windows Terminal (its taskbar pulse distracts more than it informs —
/// CC excludes it too). Unknown terminals get nothing: a wrong OSC prints
/// garbage into the scrollback.
pub fn osc_progress_capable(term_program: Option<&str>, term: Option<&str>) -> bool {
    match term_program.unwrap_or("") {
        "ghostty" | "WezTerm" | "iTerm.app" => return true,
        _ => {}
    }
    let term = term.unwrap_or("");
    term.contains("ghostty") || term.contains("wezterm")
}

/// Emit an OSC 9;4 progress state, gated on the terminal allowlist.
pub fn osc_progress(p: OscProgress) {
    static CAPABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let capable = *CAPABLE.get_or_init(|| {
        osc_progress_capable(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    });
    if capable {
        write_osc(&osc_progress_sequence(p));
    }
}

/// Strip control bytes so title text can never smuggle its own escape
/// sequences into the terminal (`;` is legal in an OSC 0 payload).
fn osc_text_clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

/// `ESC ] 0 ; title BEL` — icon name + window title, understood everywhere.
pub fn title_sequence(title: &str) -> String {
    format!("\x1b]0;{}\x07", osc_text_clean(title))
}

/// Set the terminal title (through the multiplexer wrapper).
pub fn set_title(title: &str) {
    write_osc(&title_sequence(title));
}

/// Clear the title on the way out — a dead `⚒ slag 3/9` outlives the
/// process otherwise.
pub fn clear_title() {
    write_osc(&title_sequence(""));
}

/// DEC 2026 synchronized output: terminals that buffer a frame between
/// BSU/ESU render it in one blit instead of flickering mid-draw. Never
/// inside tmux/screen — the passthrough would sync the multiplexer's own
/// redraw, not the inner pane's.
pub fn sync_output_capable(
    term_program: Option<&str>,
    term: Option<&str>,
    mux: Multiplexer,
) -> bool {
    if mux != Multiplexer::None {
        return false;
    }
    match term_program.unwrap_or("") {
        "ghostty" | "WezTerm" | "iTerm.app" | "kitty" => return true,
        _ => {}
    }
    let term = term.unwrap_or("");
    ["kitty", "alacritty", "foot", "ghostty", "wezterm", "contour"]
        .iter()
        .any(|t| term.contains(t))
}

/// Memoized capability check for the dashboard draw loop.
pub fn sync_updates_enabled() -> bool {
    static YES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *YES.get_or_init(|| {
        sync_output_capable(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
            current_multiplexer(),
        )
    })
}

/// Paint `s` in `color` when color is on; plain text otherwise. String
/// form (not a Display adapter) so render state machines can build lines
/// without touching a terminal.
pub fn paint(color: Color, s: &str) -> String {
    format!("{}{}{}", fg(color), s, reset())
}

/// Dim secondary text — tree connectors, routed-model suffixes, metadata.
pub fn dim(s: &str) -> String {
    paint(COLD, s)
}

/// Shorten to `max` characters. Counts characters, not bytes: a commission
/// written in Cyrillic or Japanese used to slice mid-codepoint and panic
/// the whole binary on `slag status`.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Heat color based on current heat level
pub fn heat_color(heat: u8) -> Color {
    match heat {
        0..=2 => WARM,
        3 => HOT,
        4 => BRIGHT,
        _ => PURE,
    }
}

/// Grade color for display
pub fn grade_color(grade: u8) -> Color {
    match grade {
        0..=1 => COLD,
        2 => HOT,
        3 => BRIGHT,
        _ => PURE,
    }
}

/// Flush stdout
pub fn flush() {
    let _ = std::io::stdout().flush();
}

// Helper to create crossterm foreground color string. Empty when color is
// off, so nothing writes escape bytes into a redirected stream.
fn fg(color: Color) -> String {
    if !colored() {
        return String::new();
    }
    SetForegroundColor(downgrade(color)).to_string()
}

fn bold() -> String {
    if !colored() {
        return String::new();
    }
    SetAttribute(Attribute::Bold).to_string()
}

fn unbold() -> String {
    if !colored() {
        return String::new();
    }
    SetAttribute(Attribute::Reset).to_string()
}

fn reset() -> String {
    if !colored() {
        return String::new();
    }
    ResetColor.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The terminal and the site are one product: these are the same five
    /// hexes as `--slag-*` in website/src/main.css. Changing the site
    /// palette without mirroring it here should fail the build, not ship a
    /// terminal that quietly disagrees with slag.dev.
    #[test]
    fn palette_matches_the_slag_dev_hexes() {
        assert_eq!(COLD, Color::Rgb { r: 0x6b, g: 0x73, b: 0x85 });
        assert_eq!(WARM, Color::Rgb { r: 0xe0, g: 0x6c, b: 0x75 });
        assert_eq!(HOT, Color::Rgb { r: 0xff, g: 0x99, b: 0x40 });
        assert_eq!(BRIGHT, Color::Rgb { r: 0xff, g: 0xd8, b: 0x66 });
        assert_eq!(PURE, Color::Rgb { r: 0xff, g: 0xff, b: 0xff });
    }

    /// `truecolor()` memoizes in a OnceLock, so one process only ever sees
    /// one branch. Assert the branch this process is actually in: both
    /// mappings stay covered across a truecolor and a plain terminal, and
    /// neither can regress unnoticed.
    #[test]
    fn downgrade_follows_terminal_capability() {
        let legacy = [
            (COLD, Color::DarkGrey),
            (WARM, Color::Red),
            (HOT, Color::AnsiValue(208)),
            (BRIGHT, Color::AnsiValue(220)),
            (PURE, Color::White),
        ];

        if truecolor() {
            for (rgb, _) in legacy {
                assert_eq!(downgrade(rgb), rgb, "24-bit terminals get the exact hex");
            }
        } else {
            for (rgb, ansi) in legacy {
                assert_eq!(downgrade(rgb), ansi, "no truecolor: {rgb:?} must degrade");
            }
        }
    }

    /// Colors outside the palette pass through untouched either way.
    #[test]
    fn downgrade_passes_through_unknown_colors() {
        assert_eq!(downgrade(Color::Green), Color::Green);
        assert_eq!(downgrade(Color::AnsiValue(42)), Color::AnsiValue(42));
    }

    #[test]
    fn heat_and_grade_colors_stay_in_the_palette() {
        let palette = [COLD, WARM, HOT, BRIGHT, PURE];
        for heat in 0u8..=8 {
            assert!(palette.contains(&heat_color(heat)), "heat {heat}");
        }
        for grade in 0u8..=6 {
            assert!(palette.contains(&grade_color(grade)), "grade {grade}");
        }
    }

    /// Spinner frames wrap forever; render loops just count ticks up.
    #[test]
    fn spinner_frame_wraps() {
        assert_eq!(spinner_frame(0), SPINNER_FRAMES[0]);
        assert_eq!(spinner_frame(SPINNER_FRAMES.len()), SPINNER_FRAMES[0]);
        assert_eq!(spinner_frame(SPINNER_FRAMES.len() + 2), SPINNER_FRAMES[2]);
    }

    /// A single redirected stream disables color on both: paint/dim feed
    /// stdout status lines AND the stderr narrator, so `2> err.log` with a
    /// TTY stdout must not leak ANSI bytes into the log (and vice versa).
    #[test]
    fn color_needs_both_streams_on_a_terminal_and_no_color_wins() {
        assert!(colored_from(false, true, true));
        assert!(!colored_from(false, true, false), "stderr redirected");
        assert!(!colored_from(false, false, true), "stdout redirected");
        assert!(!colored_from(false, false, false));
        assert!(!colored_from(true, true, true), "NO_COLOR wins");
    }

    /// With color off (redirected test output), paint/dim pass text through
    /// untouched — no escape bytes in logs or assertions.
    #[test]
    fn paint_and_dim_are_plain_when_color_is_off() {
        if !colored() {
            assert_eq!(paint(HOT, "x"), "x");
            assert_eq!(dim("meta"), "meta");
        } else {
            assert!(paint(HOT, "x").contains('x'));
            assert!(dim("meta").contains("meta"));
        }
    }

    /// Verbs wrap by turn number like spinner frames wrap by tick.
    #[test]
    fn forge_verb_wraps_and_starts_with_forging() {
        assert_eq!(forge_verb(0), "Forging");
        assert_eq!(forge_verb(FORGE_VERBS.len()), "Forging");
        assert_eq!(forge_verb(FORGE_VERBS.len() + 3), FORGE_VERBS[3]);
    }

    #[test]
    fn notify_proto_detects_terminals() {
        assert_eq!(notify_proto(Some("iTerm.app"), None), NotifyProto::Osc9);
        assert_eq!(notify_proto(Some("kitty"), None), NotifyProto::Osc99);
        assert_eq!(notify_proto(Some("WezTerm"), None), NotifyProto::Osc777);
        assert_eq!(notify_proto(Some("ghostty"), None), NotifyProto::Osc777);
        // Terminal.app has no OSC notification support — BEL only.
        assert_eq!(notify_proto(Some("Apple_Terminal"), None), NotifyProto::BelOnly);
        // TERM fallback when TERM_PROGRAM is absent or unknown.
        assert_eq!(notify_proto(None, Some("xterm-kitty")), NotifyProto::Osc99);
        assert_eq!(notify_proto(None, Some("rxvt-unicode-256color")), NotifyProto::Osc777);
        assert_eq!(notify_proto(None, Some("xterm-256color")), NotifyProto::BelOnly);
        assert_eq!(notify_proto(None, None), NotifyProto::BelOnly);
    }

    /// Every sequence leads with BEL (dings everywhere), then the OSC.
    #[test]
    fn notify_sequence_writes_bel_plus_the_right_osc() {
        let s = notify_sequence(NotifyProto::Osc9, "slag", "forge complete");
        assert!(s.starts_with('\x07'), "{s:?}");
        assert!(s.contains("\x1b]9;slag: forge complete\x07"), "{s:?}");

        let s = notify_sequence(NotifyProto::Osc99, "slag", "forge complete");
        assert!(s.contains("\x1b]99;;slag: forge complete\x1b\\"), "{s:?}");

        let s = notify_sequence(NotifyProto::Osc777, "slag", "forge complete");
        assert!(s.contains("\x1b]777;notify;slag;forge complete\x1b\\"), "{s:?}");

        assert_eq!(notify_sequence(NotifyProto::BelOnly, "slag", "x"), "\x07");
    }

    /// Notification text is data, not escape codes: ESC and `;` (the OSC
    /// 777 field separator) must not survive into the sequence body.
    #[test]
    fn notify_sequence_strips_injection_bytes_from_text() {
        let s = notify_sequence(NotifyProto::Osc777, "t\x1b]0;evil", "a;b\x07c");
        // The only ESC bytes are the two we wrote (open + ST terminator),
        // and the only `;` are the three protocol separators.
        assert_eq!(s.matches('\x1b').count(), 2, "{s:?}");
        let osc = &s[1..]; // past the leading BEL
        assert_eq!(osc.matches(';').count(), 3, "{osc:?}");
        assert!(!osc.contains('\x07'), "{osc:?}");
    }

    /// No recorded keypress = headless = idle; a fresh keypress blocks;
    /// an old one passes.
    #[test]
    fn idle_gate_blocks_active_users_and_passes_headless_runs() {
        let now = Instant::now();
        assert!(idle_enough(None, now, NOTIFY_IDLE), "headless is always idle");
        assert!(!idle_enough(Some(now), now + Duration::from_secs(2), NOTIFY_IDLE));
        assert!(idle_enough(Some(now), now + NOTIFY_IDLE, NOTIFY_IDLE));
    }

    #[test]
    fn mark_user_activity_records_a_recent_keypress() {
        mark_user_activity();
        let last = last_user_activity().expect("keypress recorded");
        assert!(
            !idle_enough(Some(last), Instant::now(), NOTIFY_IDLE),
            "a just-pressed key must block notifications"
        );
    }

    /// tmux wins over screen: `$STY` survives inside tmux-in-screen, but
    /// the innermost multiplexer is the tunnel that matters.
    #[test]
    fn detect_multiplexer_prefers_tmux_and_ignores_empty_vars() {
        assert_eq!(detect_multiplexer(None, None), Multiplexer::None);
        assert_eq!(detect_multiplexer(Some("/tmp/tmux-1"), None), Multiplexer::Tmux);
        assert_eq!(detect_multiplexer(None, Some("1234.pts-0")), Multiplexer::Screen);
        assert_eq!(detect_multiplexer(Some("/t"), Some("s")), Multiplexer::Tmux);
        // Empty exports (`TMUX=`) mean "not inside one".
        assert_eq!(detect_multiplexer(Some(""), Some("")), Multiplexer::None);
    }

    /// tmux passthrough doubles every inner ESC inside `ESC Ptmux; … ESC \`;
    /// screen wraps raw; no multiplexer passes through untouched.
    #[test]
    fn wrap_for_multiplexer_builds_dcs_envelopes() {
        let osc = "\x1b]0;slag\x07";
        assert_eq!(wrap_for_multiplexer(osc, Multiplexer::None), osc);

        let tmux = wrap_for_multiplexer(osc, Multiplexer::Tmux);
        assert_eq!(tmux, "\x1bPtmux;\x1b\x1b]0;slag\x07\x1b\\");

        let screen = wrap_for_multiplexer(osc, Multiplexer::Screen);
        assert_eq!(screen, "\x1bP\x1b]0;slag\x07\x1b\\");
    }

    #[test]
    fn osc_progress_sequence_covers_set_error_clear_and_clamps() {
        assert_eq!(osc_progress_sequence(OscProgress::Clear), "\x1b]9;4;0;\x1b\\");
        assert_eq!(osc_progress_sequence(OscProgress::Set(42)), "\x1b]9;4;1;42\x1b\\");
        assert_eq!(osc_progress_sequence(OscProgress::Error), "\x1b]9;4;2;\x1b\\");
        // A ratio rounding past 100 must not confuse the terminal.
        assert_eq!(osc_progress_sequence(OscProgress::Set(200)), "\x1b]9;4;1;100\x1b\\");
    }

    /// The allowlist minus Windows Terminal: unknown terminals (including
    /// WT, which sets no TERM_PROGRAM) get nothing.
    #[test]
    fn osc_progress_capable_follows_the_allowlist() {
        for tp in ["ghostty", "WezTerm", "iTerm.app"] {
            assert!(osc_progress_capable(Some(tp), None), "{tp}");
        }
        assert!(osc_progress_capable(None, Some("xterm-ghostty")));
        assert!(!osc_progress_capable(Some("Apple_Terminal"), None));
        assert!(!osc_progress_capable(None, Some("xterm-256color")));
        assert!(!osc_progress_capable(None, None), "Windows Terminal / unknown");
    }

    /// Title text is data: control bytes must not survive into the OSC 0
    /// payload, and the sequence must terminate with BEL.
    #[test]
    fn title_sequence_wraps_clean_text_in_osc_0() {
        assert_eq!(title_sequence("⚒ slag 3/9 forging i4"), "\x1b]0;⚒ slag 3/9 forging i4\x07");
        assert_eq!(title_sequence(""), "\x1b]0;\x07", "clear form");
        let s = title_sequence("evil\x1b]2;x\x07title");
        assert_eq!(s.matches('\x1b').count(), 1, "{s:?}");
        assert_eq!(s.matches('\x07').count(), 1, "{s:?}");
    }

    /// DEC 2026 sync: allowlisted terminals only, and never through a
    /// multiplexer — the envelope would sync tmux's redraw, not the pane's.
    #[test]
    fn sync_output_capable_gates_on_terminal_and_multiplexer() {
        for tp in ["ghostty", "WezTerm", "iTerm.app", "kitty"] {
            assert!(sync_output_capable(Some(tp), None, Multiplexer::None), "{tp}");
        }
        assert!(sync_output_capable(None, Some("alacritty"), Multiplexer::None));
        assert!(sync_output_capable(None, Some("foot-extra"), Multiplexer::None));
        assert!(!sync_output_capable(None, Some("xterm-256color"), Multiplexer::None));
        assert!(!sync_output_capable(Some("ghostty"), None, Multiplexer::Tmux), "tmux excluded");
        assert!(!sync_output_capable(Some("kitty"), None, Multiplexer::Screen), "screen excluded");
    }

    /// A Cyrillic or CJK commission used to slice mid-codepoint and abort
    /// the process on `slag status`.
    #[test]
    fn truncate_counts_characters_not_bytes() {
        let cyrillic = "сделать очень длинное описание проекта на русском";
        let cut = truncate(cyrillic, 10);
        assert_eq!(cut, "сделать оч...");
        assert_eq!(cut.chars().count(), 13);

        // Short strings pass through untouched, multibyte or not.
        assert_eq!(truncate("日本語", 10), "日本語");
        assert_eq!(truncate("ascii", 10), "ascii");
        // Exactly at the limit is not truncated.
        assert_eq!(truncate("абвгд", 5), "абвгд");
    }
}
