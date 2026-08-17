use std::io::Write;
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
        SetAttribute(Attribute::Bold),
        fg(PURE),
        title,
        reset(),
        SetAttribute(Attribute::Reset),
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
        SetAttribute(Attribute::Bold),
        fg(PURE),
        SetAttribute(Attribute::Reset),
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
pub fn key_panel(source: Option<(&str, String)>, models: &[(&str, &str)]) {
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
                "  {}key{}    {}none{}  {}run `slag key <KEY>` or set OPENROUTER_API_KEY{}",
                fg(COLD),
                reset(),
                fg(WARM),
                reset(),
                fg(COLD),
                reset()
            );
        }
    }
    for (role, model) in models {
        println!(
            "  {}{:<6}{} {}{}{}",
            fg(COLD),
            role,
            reset(),
            fg(BRIGHT),
            model,
            reset()
        );
    }
    println!();
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

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max])
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

// Helper to create crossterm foreground color string
fn fg(color: Color) -> SetForegroundColor {
    SetForegroundColor(downgrade(color))
}

fn reset() -> ResetColor {
    ResetColor
}
