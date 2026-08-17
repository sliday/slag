use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

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
