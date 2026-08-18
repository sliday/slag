use std::io::Write;
use std::time::{Duration, Instant};

use crate::config::LEDGER;
use crate::sexp::Ingot;
use crate::tui;

// ─── live spinner status line (stream mode) ─────────────────────────────
//
// One line that answers "is it doing anything?": verb, elapsed, tokens,
// and — once the sample is big enough to mean something — tok/s. The
// dashboard bottom bar renders it every frame; the stream-mode narrator
// can build the same line from its own accumulator.

/// Rate readout guards: below either threshold the tok/s figure is noise
/// (a 2-token probe over 300ms reads as "7 tok/s" and jitters wildly).
pub const RATE_MIN_ELAPSED: Duration = Duration::from_secs(5);
pub const RATE_MIN_TOKENS: u64 = 2000;

/// Tokens per second, or `None` while the sample is too small to trust.
pub fn token_rate(tokens: u64, elapsed: Duration) -> Option<u64> {
    if elapsed <= RATE_MIN_ELAPSED || tokens <= RATE_MIN_TOKENS {
        return None;
    }
    Some((tokens as f64 / elapsed.as_secs_f64()).round() as u64)
}

/// `118234` → `118k`; small counts stay exact so a 3-token probe does not
/// render as `0k`. (Mirrors the stream renderer's formatter.)
fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{}k", (n + 500) / 1000)
    }
}

/// `⚒ Forging… (12s · 4.1k tok · 38 tok/s · esc to interrupt)` — the
/// rate segment appears only past the guards.
pub fn spinner_status(verb: &str, elapsed: Duration, tokens: u64) -> String {
    let mut s = format!("⚒ {verb}… ({}s · {} tok", elapsed.as_secs(), fmt_tokens(tokens));
    if let Some(rate) = token_rate(tokens, elapsed) {
        s.push_str(&format!(" · {rate} tok/s"));
    }
    s.push_str(" · esc to interrupt)");
    s
}

/// Live status accumulator: a turn-start `Instant` plus folded
/// `EngineEvent::Tokens` deltas. The verb rotates with the turn number.
#[derive(Debug, Clone)]
pub struct LiveStatus {
    verb: &'static str,
    started: Instant,
    tokens: u64,
}

impl LiveStatus {
    /// Start a turn: pick the verb, zero the counters, stamp the clock.
    pub fn start(turn: usize) -> Self {
        Self { verb: tui::forge_verb(turn), started: Instant::now(), tokens: 0 }
    }

    /// Fold one `EngineEvent::Tokens` delta.
    pub fn add_tokens(&mut self, delta: u64) {
        self.tokens += delta;
    }

    pub fn tokens(&self) -> u64 {
        self.tokens
    }

    pub fn line(&self) -> String {
        self.line_at(Instant::now())
    }

    /// Clock-injected form for tests.
    pub fn line_at(&self, now: Instant) -> String {
        spinner_status(self.verb, now.saturating_duration_since(self.started), self.tokens)
    }
}

/// Structured progress entry for PROGRESS.md
pub struct ProgressEntry<'a> {
    pub ingot: &'a Ingot,
    pub heat: u8,
    pub files_changed: Vec<String>,
    pub learnings: Option<String>,
}

/// Append a structured progress entry to PROGRESS.md
pub fn append_entry(entry: &ProgressEntry) -> Result<(), std::io::Error> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)?;

    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M");
    writeln!(f)?;
    writeln!(
        f,
        "## {ts} [{id}] gr:{grade} skill:{skill}",
        id = entry.ingot.id,
        grade = entry.ingot.grade,
        skill = entry.ingot.skill,
    )?;
    writeln!(f, "- {}", entry.ingot.work)?;
    writeln!(f, "- heats: {}", entry.heat)?;

    if !entry.files_changed.is_empty() {
        writeln!(f, "- files: {}", entry.files_changed.join(", "))?;
    }

    if let Some(ref learnings) = entry.learnings {
        writeln!(f, "- learned: {learnings}")?;
    }

    Ok(())
}

/// Get list of files changed since last commit
pub fn files_changed_since_last_commit() -> Vec<String> {
    std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .map(|l| l.to_string())
                        .collect(),
                )
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Initialize the codebase patterns section in PROGRESS.md (Ralph-inspired)
pub fn init_patterns_section() -> Result<(), std::io::Error> {
    let ledger_path = std::path::Path::new(LEDGER);
    if ledger_path.exists() {
        let content = std::fs::read_to_string(ledger_path)?;
        if content.contains("## Codebase Patterns") {
            return Ok(());
        }
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)?;

    writeln!(f, "\n## Codebase Patterns")?;
    writeln!(f, "_Populated during forging. Helps future ingots understand the codebase._\n")?;

    Ok(())
}

/// Append a pattern observation to the patterns section
pub fn append_pattern(pattern: &str) -> Result<(), std::io::Error> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)?;

    writeln!(f, "- {pattern}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_rate_guards_noisy_early_samples() {
        // Too early: 4.1k tokens in 3s would read 1367 tok/s — jitter.
        assert_eq!(token_rate(4100, Duration::from_secs(3)), None);
        // Too few tokens: 12s elapsed but a 40-token probe.
        assert_eq!(token_rate(40, Duration::from_secs(12)), None);
        // Boundary values do not pass (spec: strictly past 5s and 2000).
        assert_eq!(token_rate(2000, Duration::from_secs(12)), None);
        assert_eq!(token_rate(4100, RATE_MIN_ELAPSED), None);
        // Past both guards: a real rate.
        assert_eq!(token_rate(4100, Duration::from_secs(108)), Some(38));
    }

    #[test]
    fn spinner_status_reads_like_the_claude_code_line() {
        let s = spinner_status("Forging", Duration::from_secs(108), 4100);
        assert_eq!(s, "⚒ Forging… (108s · 4.1k tok · 38 tok/s · esc to interrupt)");
    }

    #[test]
    fn spinner_status_hides_the_rate_until_the_sample_is_trustworthy() {
        let s = spinner_status("Smelting", Duration::from_secs(2), 120);
        assert_eq!(s, "⚒ Smelting… (2s · 120 tok · esc to interrupt)");
        assert!(!s.contains("tok/s"));
    }

    #[test]
    fn live_status_folds_token_deltas_from_a_turn_start() {
        let mut live = LiveStatus::start(0);
        live.add_tokens(1500);
        live.add_tokens(2600);
        assert_eq!(live.tokens(), 4100);

        let line = live.line_at(live.started + Duration::from_secs(10));
        assert!(line.starts_with("⚒ Forging… (10s · 4.1k tok · "), "{line}");
        assert!(line.contains("tok/s"), "past both guards the rate shows: {line}");
        assert!(line.ends_with("esc to interrupt)"), "{line}");

        // A fresh turn resets counters and rotates the verb.
        let next = LiveStatus::start(1);
        assert_eq!(next.tokens(), 0);
        assert!(next.line_at(next.started).starts_with("⚒ Smelting…"));
    }

    #[test]
    fn fmt_tokens_scales_readably() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(4100), "4.1k");
        assert_eq!(fmt_tokens(118_234), "118k");
    }
}
