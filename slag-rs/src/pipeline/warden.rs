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

mod grounding;
pub(crate) use grounding::{InspectionLog, parse_grounded_verdict};

/// How many warden rounds this run was armed with. Ambient like
/// `tui::quiet` because it is one run-level integer read deep in the forge:
/// threading it through four signatures to reach a single `if` costs more
/// clarity than it buys.
static ROUNDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static REVIEW_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub(crate) fn set_review_base(base: Option<String>) {
    *REVIEW_BASE.lock().unwrap_or_else(|e| e.into_inner()) = base.map(|hash| hash.trim().to_string()).filter(|hash| {
        matches!(hash.len(), 40 | 64) && hash.bytes().all(|c| c.is_ascii_hexdigit())
    });
}

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
    let prompt = format!("{}\n\n{}", workspace_context(), crate::flux::bar_prompt(&ore, &blueprint));
    // A model that explores with tools ends its turn on a finish summary
    // rather than the document, and "acceptance bar established" is exactly
    // the adjective this whole mechanism exists to avoid. Ask again plainly
    // before giving up: refusing outright disabled every goal check for a
    // whole run and let it report success unjudged.
    let raw = crate::smith::invoke_document(
        smith.as_ref(),
        &prompt,
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
    let prompt = format!("{}\n\n{}\n\nCOORDINATOR PROOF OUTPUT (already executed before review):\n{}",
        workspace_context(), crate::flux::warden_prompt(&ore, &bar), proof_output());
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

fn workspace_context() -> String {
    let base = REVIEW_BASE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    workspace_context_in(std::path::Path::new("."), base.as_deref())
}

fn product_path(path: &str) -> bool {
    let path = std::path::Path::new(path);
    !super::COORDINATOR_FILES.iter().any(|name| path == std::path::Path::new(name))
        && !path.starts_with("logs")
        && !path.components().any(|part| {
            let name = part.as_os_str().to_string_lossy();
            matches!(name.as_ref(), ".git" | ".slag" | "__pycache__" | "node_modules" | ".venv" | "target")
                || name.starts_with("slag-anvil-")
        })
}

fn split_paths(bytes: &[u8]) -> std::collections::BTreeSet<String> {
    bytes.split(|b| *b == 0).filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).trim_start_matches("./").to_string())
        .filter(|path| product_path(path)).collect()
}

fn git_paths(root: &std::path::Path, args: &[&str]) -> Option<std::collections::BTreeSet<String>> {
    let output = crate::engine::sandbox::git().args(args).current_dir(root).output().ok()?;
    output.status.success().then(|| split_paths(&output.stdout))
}

fn context_paths(paths: Option<&std::collections::BTreeSet<String>>) -> String {
    let Some(paths) = paths else { return "Unavailable; inspect the workspace with read-only tools.".into() };
    if paths.is_empty() { return "None.".into() }
    let mut output = String::new();
    for (index, path) in paths.iter().enumerate() {
        let line = format!("- {}\n", serde_json::to_string(path).unwrap());
        if index >= 120 || output.len() + line.len() > 4_000 {
            output.push_str(&format!("... {} more paths; inspect the relevant directories.\n", paths.len() - index));
            break;
        }
        output.push_str(&line);
    }
    output
}

fn workspace_context_in(root: &std::path::Path, base: Option<&str>) -> String {
    let files = std::process::Command::new("rg")
        .args(["--no-config", "--files", "--hidden", "--no-require-git", "--null",
            "-g", "!/logs/**", "-g", "!**/.git/**", "-g", "!**/.slag/**",
            "-g", "!**/slag-anvil-*/**", "-g", "!**/__pycache__/**",
            "-g", "!**/node_modules/**", "-g", "!**/.venv/**", "-g", "!**/target/**"])
        .current_dir(root).output().ok()
        .filter(|output| output.status.success() || output.status.code() == Some(1))
        .map(|output| split_paths(&output.stdout))
        .or_else(|| git_paths(root, &["ls-files", "--cached", "--others", "--exclude-standard", "--exclude-per-directory=.ignore", "-z"]));
    let changed = git_paths(root, &["diff", "--name-only", "--no-renames", "-z", base.unwrap_or("HEAD"), "--"])
        .map(|paths| paths.into_iter().filter(|path| {
            !root.join(path).exists() || files.as_ref().is_none_or(|files| files.contains(path))
        }).collect());
    let untracked = git_paths(root, &["ls-files", "--others", "--exclude-standard", "-z"])
        .map(|paths| paths.into_iter().filter(|path| files.as_ref().is_none_or(|files| files.contains(path))).collect());
    let changes = base.map(|hash| format!("CHANGES SINCE COMMISSION BASE {hash}"))
        .unwrap_or_else(|| "BASELINE UNAVAILABLE: tracked changes below cover only the index and working tree".into());
    format!(
        "WORKSPACE TREE (product file paths; ignore rules and heap exclusions apply):\n{}\n\n\
         {changes}:\n{}\n\n\
         UNTRACKED PRODUCT FILES (commission ownership unknown):\n{}\n\n\
         Paths describe scope, not inspection evidence. Read the relevant files before you judge them.",
        context_paths(files.as_ref()), context_paths(changed.as_ref()), context_paths(untracked.as_ref()),
    )
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
    let prompt = format!("{}\n\n{}\n\nDOCUMENT PATH: {doc_path}\nRead this file to obtain inspection evidence.",
        workspace_context(), crate::flux::plan_warden_prompt(kind, &ore, &bar, &doc));
    match smith.invoke(&prompt).await {
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
    let prompt = format!("{}\n\n{}\n\nPROOF COMMAND: {}\nCOORDINATOR PROOF OUTPUT (already executed before review):\n{}",
        workspace_context(), crate::flux::ingot_warden_prompt(&ingot.work, &ingot.bar), ingot.proof, proof_output());
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

    fn git(root: &std::path::Path, args: &[&str]) -> String {
        let output = crate::engine::sandbox::git().args(args).current_dir(root)
            .env("GIT_AUTHOR_NAME", "slag test").env("GIT_AUTHOR_EMAIL", "test@slag.test")
            .env("GIT_COMMITTER_NAME", "slag test").env("GIT_COMMITTER_EMAIL", "test@slag.test")
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn workspace_context_keeps_committed_changes_and_prunes_ignored_heap_files() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        std::fs::write(root.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.path().join(".ignore"), "scratch/\n").unwrap();
        for name in ["old.py", "deleted.py", "README.md"] {
            std::fs::write(root.path().join(name), "before\n").unwrap();
        }
        git(root.path(), &["add", "--", ".gitignore", ".ignore", "old.py", "deleted.py", "README.md"]);
        git(root.path(), &["commit", "-m", "baseline"]);
        let base = git(root.path(), &["rev-parse", "HEAD"]);
        std::fs::write(root.path().join("old.py"), "after\n").unwrap();
        std::fs::remove_file(root.path().join("deleted.py")).unwrap();
        for name in ["pkg/main.py", "pkg/logs/events.txt", "pkg/PRD.md", "pyproject.toml"] {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "product\n").unwrap();
        }
        git(root.path(), &["add", "-A"]);
        git(root.path(), &["commit", "-m", "forge product"]);
        for name in ["new.txt", "PLAN.md", "ignored.txt", "logs/session.txt", "scratch/notes.txt",
            "slag-anvil-old/private.py", "pkg/slag-anvil-old/private.py", "target/cache.py",
            ".slag/private.py", "__pycache__/cache.py", "node_modules/dependency.py"]
        {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "private\n").unwrap();
        }
        let context = workspace_context_in(root.path(), Some(&base));
        let tree = context.split("CHANGES SINCE COMMISSION BASE").next().unwrap();
        assert!(tree.contains("pkg/main.py"));
        assert!(tree.contains("pkg/logs/events.txt"));
        assert!(tree.contains("pkg/PRD.md"));
        assert!(!tree.contains("deleted.py"));
        for hidden in ["PLAN.md", "ignored.txt", "session.txt", "scratch/notes.txt", "private.py", "cache.py", "dependency.py"] {
            assert!(!context.contains(hidden), "{hidden} leaked into {context}");
        }
        let changes = context.split("CHANGES SINCE COMMISSION BASE").nth(1).unwrap()
            .split("UNTRACKED PRODUCT FILES").next().unwrap();
        for changed in ["old.py", "deleted.py", "pkg/main.py", "pyproject.toml"] {
            assert!(changes.contains(changed), "{changed} missing from {changes}");
        }
        assert!(!changes.contains("README.md"));
        assert!(context.contains("UNTRACKED PRODUCT FILES (commission ownership unknown):\n- \"new.txt\""));
        let fallback = workspace_context_in(root.path(), None);
        assert!(fallback.contains("BASELINE UNAVAILABLE"));
        let changes = fallback.split("BASELINE UNAVAILABLE").nth(1).unwrap()
            .split("UNTRACKED PRODUCT FILES").next().unwrap();
        assert!(!changes.contains("old.py"));
    }

    #[test]
    fn workspace_paths_have_bounded_size_and_escape_control_characters() {
        let paths = ["src/a\nmisleading.py".to_string()].into_iter().collect();
        assert_eq!(context_paths(Some(&paths)), "- \"src/a\\nmisleading.py\"\n");
        let paths = (0..500).map(|n| format!("src/file_{n:04}.py")).collect();
        let rendered = context_paths(Some(&paths));
        assert!(rendered.len() < 4_100);
        assert!(rendered.contains("380 more paths"));
    }

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
