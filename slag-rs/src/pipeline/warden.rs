//! The warden: a fresh-context critic that judges whether the goal was met.
//!
//! `:proof` answers "did this task's artifact appear". Nothing answered "does
//! the thing we were asked for exist", and the finish summary that claimed it
//! was written by the smith that did the work — the builder grading itself.
//! The warden supplies the two pieces that were missing: a bar the work is
//! measured against, and a judge that never built anything.
//!
//! It is deliberately given tools. A warden reasoning from a summary is
//! grading a summary; this one runs the build, reads the files, and reports
//! what it actually saw.

use crate::config::{BLUEPRINT, EngineConfig, LEDGER, ORE_FILE};
use crate::smith::EngineHooks;
use crate::error::SlagError;
use crate::tui;

/// Where the derived acceptance bar lives, once, for every warden to read.
pub const BAR_FILE: &str = "BAR.md";

/// A warden's answer. Structured on purpose: a prose verdict drifts into
/// flattery, and the caller needs to branch on the result, not read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Did the artifact meet the bar?
    pub fulfilled: bool,
    /// The single biggest gap that still matters. Empty when fulfilled.
    /// One gap, not a list: twenty small notes produce twenty small edits
    /// and no jump in quality.
    pub gap: String,
    /// What the warden actually inspected — a path, a number, an
    /// observation. A verdict with no evidence is an opinion.
    pub evidence: String,
}

/// Parse the warden's report. Tolerant of surrounding prose, because a
/// model asked for three lines often delivers three lines and a paragraph.
///
/// A report with no readable VERDICT is treated as a loss: the run must not
/// be able to pass the goal check by producing unparseable output.
pub fn parse_verdict(raw: &str) -> Verdict {
    let mut fulfilled = None;
    let mut gap = String::new();
    let mut evidence = String::new();
    for line in raw.lines() {
        let line = line.trim().trim_start_matches(['#', '*', '-', ' ']);
        let Some((key, value)) = line.split_once(':') else { continue };
        // Models decorate. `**VERDICT:** fail` leaves the bold on the value.
        let value = value.trim_matches(|c: char| c == '*' || c == '_' || c == '`').trim();
        let key = key.trim_matches(|c: char| c == '*' || c == '_' || c == '`').trim();
        match key.to_ascii_uppercase().as_str() {
            "VERDICT" => {
                // The whole word, not a prefix: `passable-ish` is a hedge,
                // and a hedge must not clear the goal gate.
                let word = value
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_matches(|c: char| !c.is_ascii_alphabetic())
                    .to_ascii_lowercase();
                match word.as_str() {
                    "pass" => fulfilled = Some(true),
                    "fail" => fulfilled = Some(false),
                    _ => {}
                }
            }
            "GAP" if gap.is_empty() => gap = value.to_string(),
            "EVIDENCE" if evidence.is_empty() => evidence = value.to_string(),
            _ => {}
        }
    }
    match fulfilled {
        Some(true) => Verdict { fulfilled: true, gap: String::new(), evidence },
        Some(false) => Verdict {
            fulfilled: false,
            gap: if gap.is_empty() {
                "the warden reported a failure without naming a gap".to_string()
            } else {
                gap
            },
            evidence,
        },
        None => Verdict {
            fulfilled: false,
            gap: "the warden returned no readable verdict".to_string(),
            evidence,
        },
    }
}

/// Read the acceptance bar, deriving and storing it on first use.
///
/// Derived once rather than per round so every warden in a run judges
/// against the same statement; a bar that moves between rounds cannot be
/// lost against.
pub async fn bar(cfg: &EngineConfig, hooks: &EngineHooks) -> Result<String, SlagError> {
    let path = std::path::Path::new(BAR_FILE);
    if let Ok(existing) = std::fs::read_to_string(path) {
        if !existing.trim().is_empty() {
            return Ok(existing);
        }
    }
    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "No blueprint".into());

    tui::header("WARDEN · setting the bar");
    let smith = crate::smith::make_plan_smith(cfg, hooks, crate::engine::Role::Judge);
    let raw = smith
        .invoke(&crate::flux::bar_prompt(&ore, &blueprint))
        .await
        .map_err(|e| SlagError::SurveyFailed(e.to_string()))?;
    // A model that explores with tools ends its turn on a finish summary
    // rather than the document, and "acceptance bar established" is exactly
    // the adjective this whole mechanism exists to avoid. Judging against
    // it would look like a goal check while being none, so an unusable bar
    // is refused rather than quietly used.
    if !is_usable(&raw) {
        return Err(SlagError::SurveyFailed(
            "the bar came back as a summary, not a checklist — nothing inspectable to judge against"
                .to_string(),
        ));
    }
    let _ = std::fs::write(path, &raw);
    tui::status_line("=", tui::COLD, "Bar set");
    Ok(raw)
}

/// A bar is usable when a reviewer could actually work from it: more than a
/// sentence, and carrying at least one line that names a condition.
fn is_usable(bar: &str) -> bool {
    let lines = bar.lines().filter(|l| !l.trim().is_empty()).count();
    let has_items = bar
        .lines()
        .any(|l| {
            let t = l.trim_start();
            t.starts_with("- [") || t.starts_with("- ") || t.starts_with("* ")
        });
    lines >= 3 && has_items
}

/// Judge the built artifact against the bar.
///
/// The smith is a forge-mode agent because the warden has to *run* things —
/// the build, the tests, the app. What it must not get is the builder's
/// account of the work, so nothing about the forge is passed in beyond the
/// goal and the bar.
pub async fn judge_artifact(
    cfg: &EngineConfig,
    hooks: &EngineHooks,
    round: usize,
) -> Result<Verdict, SlagError> {
    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;
    let bar = bar(cfg, hooks).await?;

    tui::header(&format!("WARDEN · round {round}"));
    let smith = crate::smith::make_smith(cfg, "default", crate::config::HIGH_GRADE, hooks);
    let raw = smith
        .invoke(&crate::flux::warden_prompt(&ore, &bar))
        .await
        .map_err(|e| SlagError::SurveyFailed(e.to_string()))?;
    let verdict = parse_verdict(&raw);
    record(round, &verdict);
    Ok(verdict)
}

/// Append the verdict to the ledger. `PROGRESS.md` is the window into a long
/// run: a reader should be able to open one file and see what each round
/// rejected and why, without interrupting the forge.
fn record(round: usize, v: &Verdict) {
    use std::io::Write;
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(LEDGER) else {
        return;
    };
    let outcome = if v.fulfilled { "goal met" } else { "goal not met" };
    let _ = writeln!(
        f,
        "\n## Warden round {round} — {outcome}\n\n- gap: {}\n- evidence: {}",
        if v.gap.is_empty() { "—" } else { &v.gap },
        if v.evidence.is_empty() { "—" } else { &v.evidence },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_line_bar_is_refused() {
        // What a tool-using model actually returned: its finish summary.
        // Judged against, it would pass anything.
        assert!(!is_usable("Acceptance bar established for the calculator with pass/fail checklist."));
        assert!(!is_usable(""));
        assert!(!is_usable("## THE BAR\nIt should be good."));
    }

    #[test]
    fn a_checklist_bar_is_usable() {
        assert!(is_usable(
            "## THE BAR\nMatches a POSIX calculator.\n\n## CHECKLIST\n- [ ] `calc '2+3'` prints 5\n- [ ] divide by zero exits non-zero"
        ));
    }

    #[test]
    fn a_hedged_verdict_is_not_a_pass() {
        // The gate must not be clearable by sounding positive.
        for raw in [
            "VERDICT: passable",
            "VERDICT: passes most of the bar",
            "VERDICT: pass-ish",
            "VERDICT: probably fine",
        ] {
            assert!(!parse_verdict(raw).fulfilled, "{raw} cleared the goal gate");
        }
    }

    #[test]
    fn markdown_decoration_does_not_hide_the_verdict() {
        let v = parse_verdict("**VERDICT:** pass\n**EVIDENCE:** `npm test` 24/24");
        assert!(v.fulfilled, "bold must not make a verdict unreadable");
    }

    #[test]
    fn the_first_gap_wins_when_a_critic_lists_several() {
        // One gap per round is the rule; a critic that lists more gets
        // read for the first, not merged into a to-do list.
        let v = parse_verdict("VERDICT: fail\nGAP: no audio\nGAP: no enemies\nEVIDENCE: src/");
        assert_eq!(v.gap, "no audio");
    }

    #[test]
    fn a_pass_carries_no_gap() {
        let v = parse_verdict("VERDICT: pass\nEVIDENCE: npm test 24/24, build clean");
        assert!(v.fulfilled);
        assert!(v.gap.is_empty());
        assert!(v.evidence.contains("24/24"));
    }

    #[test]
    fn a_failure_keeps_the_one_gap_and_its_evidence() {
        let v = parse_verdict(
            "VERDICT: fail\nGAP: enemies never path around cover\nEVIDENCE: src/ai/bot.ts:88 has no navmesh query",
        );
        assert!(!v.fulfilled);
        assert_eq!(v.gap, "enemies never path around cover");
        assert!(v.evidence.contains("bot.ts:88"));
    }

    #[test]
    fn prose_around_the_report_is_tolerated() {
        let v = parse_verdict(
            "I ran the build and looked at the page.\n\n**VERDICT:** fail\n- GAP: no audio at all\n- EVIDENCE: src/audio/ is empty\n\nHappy to explain further.",
        );
        assert!(!v.fulfilled);
        assert_eq!(v.gap, "no audio at all");
    }

    #[test]
    fn an_unreadable_report_is_a_loss_not_a_pass() {
        // The run must not be able to clear the goal check by returning
        // something the parser cannot read.
        let v = parse_verdict("Looks great to me!");
        assert!(!v.fulfilled);
        assert!(v.gap.contains("no readable verdict"), "{}", v.gap);
    }

    #[test]
    fn a_failure_without_a_named_gap_still_reads_as_a_failure() {
        let v = parse_verdict("VERDICT: fail");
        assert!(!v.fulfilled);
        assert!(!v.gap.is_empty(), "a gapless failure still has to say something");
    }

    #[test]
    fn only_an_explicit_pass_passes() {
        for raw in ["VERDICT: passable-ish", "VERDICT: mostly", "VERDICT: unclear"] {
            let v = parse_verdict(raw);
            assert!(!v.fulfilled, "{raw} must not read as a pass");
        }
    }
}
