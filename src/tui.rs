use std::io::Write;

use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use indicatif::{ProgressBar, ProgressStyle};

use crate::crucible::CrucibleCounts;

// Palette (cold ore → hot metal → pure steel)
pub const COLD: Color = Color::DarkGrey;
pub const WARM: Color = Color::Red;
pub const HOT: Color = Color::AnsiValue(208); // orange
pub const BRIGHT: Color = Color::AnsiValue(220); // yellow
pub const PURE: Color = Color::White;

pub fn hr() {
    println!(
        "{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        fg(COLD),
        reset()
    );
}

pub fn header(title: &str) {
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
    println!("  {}{}{} {}", fg(color), icon, reset(), msg);
}

pub fn show_banner() {
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
    println!("  {}survey · design · forge · temper{}", fg(COLD), reset());
}

pub fn ingot_status_line(counts: &CrucibleCounts) {
    let total = counts.total.max(1);
    let pct = counts.forged * 100 / total;
    print!("{}[{}", fg(COLD), reset());
    print!(" ✅{} ", counts.forged);
    print!("{}🔥{}{} ", fg(HOT), counts.molten, reset());
    print!("{}🧱{}{}", fg(COLD), counts.ore, reset());
    if counts.cracked > 0 {
        print!(" {}❌{}{}", fg(WARM), counts.cracked, reset());
    }
    print!("{}]{} {}{}%{}", fg(COLD), reset(), fg(PURE), pct, reset());
}

pub fn temper_bar(counts: &CrucibleCounts) {
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
    println!("{}]{} {}{}%{}", fg(COLD), reset(), fg(PURE), pct, reset());
}

/// Create a spinner for long operations
pub fn spinner(msg: &str) -> ProgressBar {
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

/// Create a heat bar visualization like [▪▪▫▫▫]
pub fn heat_bar(current: u8, max: u8) -> String {
    let mut bar = String::from("[");
    for i in 1..=max {
        if i <= current {
            bar.push('▪');
        } else {
            bar.push('▫');
        }
    }
    bar.push(']');
    bar
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

/// Show the legend for ingot status emoji
pub fn show_legend() {
    println!(
        "  {}LEGEND:{} 🧱 queued  🔥 forging  ✅ done  ❌ failed",
        fg(COLD),
        reset()
    );
}

/// Format elapsed time as "Xm YYs" or "Xs"
pub fn format_elapsed(secs: u64) -> String {
    let mins = secs / 60;
    let remaining_secs = secs % 60;
    if mins > 0 {
        format!("{mins}m{remaining_secs:02}s")
    } else {
        format!("{secs}s")
    }
}

// Helper to create crossterm foreground color string
fn fg(color: Color) -> SetForegroundColor {
    SetForegroundColor(color)
}

fn reset() -> ResetColor {
    ResetColor
}
