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

/// How many warden rounds this run was armed with. Ambient like
/// `tui::quiet` because it is one run-level integer read deep in the forge:
/// threading it through four signatures to reach a single `if` costs more
/// clarity than it buys.
static ROUNDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Arm the goal checks for this run. 0 leaves every warden asleep.
pub fn set_rounds(n: usize) {
    ROUNDS.store(n, std::sync::atomic::Ordering::Relaxed);
}

/// Whether goal checking is armed.
pub fn armed() -> bool {
    ROUNDS.load(std::sync::atomic::Ordering::Relaxed) > 0
}

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

impl Verdict {
    pub fn unverified(reason: impl AsRef<str>) -> Self {
        let reason = reason.as_ref().trim();
        Self {
            fulfilled: false,
            gap: if is_unverified_report(reason) {
                reason.to_string()
            } else {
                format!("{reason}; goal unverified")
            },
            evidence: String::new(),
        }
    }

    pub fn is_unverified(&self) -> bool {
        !self.fulfilled && is_unverified_report(&self.gap)
    }
}

pub(crate) fn is_unverified_report(report: &str) -> bool {
    report.trim_end().ends_with("; goal unverified")
}

/// Parse the warden's report. Tolerant of surrounding prose, because a
/// model asked for three lines often delivers three lines and a paragraph.
///
/// An unreadable report leaves the goal unverified without rejecting the artifact.
pub fn parse_verdict(raw: &str) -> Verdict {
    let mut fulfilled = None;
    let mut unverified = false;
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
                    "unverified" => unverified = true,
                    _ => {}
                }
            }
            "GAP" if gap.is_empty() => gap = value.to_string(),
            "EVIDENCE" if evidence.is_empty() => evidence = value.to_string(),
            _ => {}
        }
    }
    if unverified {
        let mut verdict = Verdict::unverified(if gap.is_empty() { "the warden could not complete its inspection" } else { &gap });
        verdict.evidence = evidence;
        return verdict;
    }
    match fulfilled {
        Some(true) if !evidence.trim().is_empty() => Verdict { fulfilled: true, gap: String::new(), evidence },
        Some(true) => Verdict::unverified("the warden returned a pass without inspection evidence"),
        Some(false) if gap.is_empty() => Verdict::unverified("the warden reported a failure without naming a gap"),
        Some(false) => Verdict { fulfilled: false, gap, evidence },
        None => Verdict::unverified("the warden returned no readable verdict"),
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
    let smith = crate::smith::make_plan_smith(cfg, hooks, crate::engine::Role::Warden);
    // A model that explores with tools ends its turn on a finish summary
    // rather than the document, and "acceptance bar established" is exactly
    // the adjective this whole mechanism exists to avoid. Ask again plainly
    // before giving up: refusing outright disabled every goal check for a
    // whole run and let it report success unjudged.
    let raw = crate::smith::invoke_document(
        smith.as_ref(),
        &crate::flux::bar_prompt(&ore, &blueprint),
        crate::flux::DOCUMENT_ONLY_REMINDER,
        is_usable,
    )
    .await
    .map_err(SlagError::SurveyFailed)?;
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
/// The warden reads source and runs checks with a bounded inspection budget.
/// It receives the goal and bar without the builder's account of the work.
pub async fn judge_artifact(
    cfg: &EngineConfig,
    hooks: &EngineHooks,
    round: usize,
) -> Result<Verdict, SlagError> {
    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;
    let bar = match bar(cfg, hooks).await {
        Ok(bar) => bar,
        Err(e) => {
            record(round, &Verdict::unverified(format!("the warden could not establish the acceptance bar: {e}")));
            return Err(e);
        }
    };

    tui::header(&format!("WARDEN · round {round}"));
    let smith = crate::smith::make_warden(cfg, hooks);
    let prompt = format!("{}\n\nCOORDINATOR PROOF OUTPUT (already executed before review):\n{}",
        crate::flux::warden_prompt(&ore, &bar), proof_output());
    let raw = match smith
        .invoke(&prompt)
        .await
    {
        Ok(raw) => raw,
        Err(e) => {
            let verdict = Verdict::unverified(format!("the warden could not inspect the artifact: {e}"));
            record(round, &verdict);
            if e.is_deterministic_provider() {
                return Err(e);
            }
            return Ok(verdict);
        }
    };
    let verdict = parse_verdict(&raw);
    record(round, &verdict);
    Ok(verdict)
}

fn proof_output() -> String {
    let mut paths: Vec<_> = std::fs::read_dir(crate::config::LOG_DIR).into_iter().flatten()
        .filter_map(Result::ok).map(|entry| entry.path())
        .filter(|path| path.file_name().and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("_ASSAY_") && name.ends_with(".log")))
        .collect();
    paths.sort();
    paths.into_iter().rev().take(8).rev().filter_map(|path| {
        let text = std::fs::read_to_string(&path).ok()?;
        let tail: String = text.chars().rev().take(4_000).collect::<String>().chars().rev().collect();
        Some(format!("{}:\n{tail}", path.display()))
    }).collect::<Vec<_>>().join("\n\n")
}

/// Judge a plan document -- the blueprint, or the crucible -- against the
/// bar before the forge runs.
///
/// Plan-mode on purpose: there is no artifact to execute yet, so this is a
/// reading job, and a reading job should not pay for a forge-mode session.
pub async fn judge_plan_doc(
    cfg: &EngineConfig,
    hooks: &EngineHooks,
    kind: &str,
    doc_path: &str,
) -> Result<Verdict, SlagError> {
    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;
    let doc = std::fs::read_to_string(doc_path).unwrap_or_default();
    if doc.trim().is_empty() {
        return Ok(Verdict::unverified(format!("{doc_path} is empty; nothing to judge")));
    }
    let bar = bar(cfg, hooks).await?;

    tui::header(&format!("WARDEN · {kind}"));
    let smith = crate::smith::make_plan_smith(cfg, hooks, crate::engine::Role::Warden);
    match smith.invoke(&crate::flux::plan_warden_prompt(kind, &ore, &bar, &doc)).await {
        Ok(raw) => Ok(parse_verdict(&raw)),
        Err(e) if e.is_deterministic_provider() => Err(e),
        Err(e) => Ok(Verdict::unverified(format!("the warden could not inspect {doc_path}: {e}"))),
    }
}

/// Judge one ingot against its own bar.
///
/// The run-level warden asks whether the commission was met; this asks the
/// same question of a sub-goal, and the answer rides the ladder the forge
/// already has. A loss returns the gap as a plain failure string, so it
/// feeds the next heat exactly as a failed proof does -- and when the heats
/// run out, the ingot cracks into re-smelt, which splits it into sub-ingots
/// that get judged the same way. Same check at every depth.
pub async fn judge_ingot(
    cfg: &EngineConfig,
    hooks: &EngineHooks,
    ingot: &crate::sexp::Ingot,
) -> Verdict {
    let smith = crate::smith::make_warden(cfg, hooks);
    let prompt = format!("{}\n\nPROOF COMMAND: {}\nCOORDINATOR PROOF OUTPUT (already executed before review):\n{}",
        crate::flux::ingot_warden_prompt(&ingot.work, &ingot.bar), ingot.proof, proof_output());
    match smith.invoke(&prompt).await {
        Ok(raw) => parse_verdict(&raw),
        Err(e) => Verdict::unverified(format!("the warden could not inspect the ingot: {e}")),
    }
}

/// Append the verdict to the ledger. `PROGRESS.md` is the window into a long
/// run: a reader should be able to open one file and see what each round
/// rejected and why, without interrupting the forge.
fn record(round: usize, v: &Verdict) {
    use std::io::Write;
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(LEDGER) else {
        return;
    };
    let outcome = if v.fulfilled { "goal met" } else if v.is_unverified() { "goal unverified" } else { "goal not met" };
    let _ = writeln!(
        f,
        "\n## Warden round {round}: {outcome}\n\n- gap: {}\n- evidence: {}",
        if v.gap.is_empty() { "—" } else { &v.gap },
        if v.evidence.is_empty() { "—" } else { &v.evidence },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_unverified_report_keeps_the_inspection_limit() {
        let verdict = parse_verdict("VERDICT: unverified\nGAP: fixtures require writes\nEVIDENCE: read tests/test_app.py");
        assert!(verdict.is_unverified());
        assert_eq!(verdict.gap, "fixtures require writes; goal unverified");
        assert_eq!(verdict.evidence, "read tests/test_app.py");
    }

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
    fn a_pass_without_inspection_evidence_leaves_the_goal_unverified() {
        let v = parse_verdict("VERDICT: pass\nGAP:\nEVIDENCE:");
        assert!(!v.fulfilled);
        assert!(v.is_unverified());
        assert!(v.gap.contains("without inspection evidence"));
    }

    #[test]
    fn a_failure_keeps_the_one_gap_and_its_evidence() {
        let v = parse_verdict(
            "VERDICT: fail\nGAP: enemies never path around cover\nEVIDENCE: src/ai/bot.ts:88 has no navmesh query",
        );
        assert!(!v.fulfilled);
        assert!(!v.is_unverified());
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
    fn an_unreadable_report_leaves_the_goal_unverified() {
        let v = parse_verdict("Looks great to me!");
        assert!(!v.fulfilled);
        assert!(v.is_unverified());
        assert_eq!(v.gap, "the warden returned no readable verdict; goal unverified");
    }

    #[test]
    fn a_failure_without_a_named_gap_leaves_the_goal_unverified() {
        let v = parse_verdict("VERDICT: fail");
        assert!(!v.fulfilled);
        assert!(v.is_unverified());
        assert!(v.gap.contains("without naming a gap"));
    }

    #[test]
    fn a_failure_with_a_named_gap_remains_actionable_without_evidence() {
        let v = parse_verdict("VERDICT: fail\nGAP: wrong word count");
        assert!(!v.fulfilled);
        assert!(!v.is_unverified());
        assert_eq!(v.gap, "wrong word count");
    }

    #[test]
    fn the_unverified_marker_does_not_repeat() {
        let first = Verdict::unverified("inspection failed");
        let second = Verdict::unverified(&first.gap);
        assert_eq!(first, second);
        assert!(is_unverified_report(&first.gap));
        assert!(!is_unverified_report("a required output is missing"));
    }

    #[test]
    fn only_an_explicit_pass_passes() {
        for raw in ["VERDICT: passable-ish", "VERDICT: mostly", "VERDICT: unclear"] {
            let v = parse_verdict(raw);
            assert!(!v.fulfilled, "{raw} must not read as a pass");
        }
    }
}
