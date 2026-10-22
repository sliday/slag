//! `slag fix`: solve one issue in an existing repository, fast, and prove it.
//!
//! The commission pipeline (survey, found, forge, assay) builds a project
//! from nothing. An issue in a repository that already has code and tests
//! needs none of that ceremony, and the ingot machinery would write PRD,
//! BLUEPRINT, and PLAN files into the user's root. So a fix is one agent
//! session per attempt:
//!
//! 1. slag gathers what it can without a model, concurrently: the issue
//!    (fetched with `gh` when it names one), the test command, and how the
//!    tests end before any edit.
//! 2. One forge-mode session reads, reproduces, fixes, and runs the tests.
//! 3. slag reruns the tests itself. The verdict comes from those exit
//!    codes, never from the session's summary.
//! 4. A failed attempt retries on the plan model, starting from the failure
//!    slag observed and the changes already on disk.
//! 5. A passing fix gets its reproduction checked (the new test must fail
//!    with the source change reverted) and lands as one commit on a fresh
//!    `slag/fix-*` branch, or a draft PR with `--pr`.

mod evolve;
mod issue;
mod repo;

pub(crate) use issue::slug;

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::cli::FixArgs;
use crate::config::{EngineConfig, HIGH_GRADE};
use crate::engine::{emit, stats, EngineEvent};
use crate::error::SlagError;
use crate::smith::EngineHooks;
use crate::tui;

use issue::Issue;
use repo::{Git, Run, TestCmd};

use super::{bold, reset};

/// The id the fix wears on the event stream, so the dashboard shows it as
/// one ingot with one heat per attempt.
const FIX_ID: &str = "fix";
/// Ceiling for any one test run slag makes. A suite slower than this is
/// not one a quick fix can afford to run three times.
const TEST_TIMEOUT: Duration = Duration::from_secs(300);
/// How much of the repository's file list reaches the prompt.
const FILE_LIST_CAP: usize = 200;
const FILE_LIST_BYTES: usize = 8_000;
/// How much of an instruction file (AGENTS.md, CLAUDE.md) the prompt gets.
const INSTRUCTIONS_BYTES: usize = 4_000;
/// Test output the prompt and the report carry: the end, where the
/// verdict and the failing assertion are.
const OUTPUT_TAIL_LINES: usize = 40;
const OUTPUT_TAIL_BYTES: usize = 4_000;
/// A retry starts from the previous attempt's diff, capped.
const RETRY_DIFF_BYTES: usize = 8_000;
/// How much of the diff the reviewer reads.
const REVIEW_DIFF_BYTES: usize = 16_000;
/// How long the issue's own command may run before slag stops waiting.
const ISSUE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest a review may take before the verified fix lands without it.
const REVIEW_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the without-the-fix check may run. The swap rebuilds the
/// original code, a cost the incremental passing run never paid, so the
/// check gets three times the slower of the passing run and the baseline
/// run before any edit (which compiled the original code too), with a
/// slow-starter floor, capped by the usual ceiling. A larger factor would
/// make a slow suite (38s) wait out the whole ceiling on every hang. A baseline that
/// is missing or timed out says nothing about the rebuild: the check gets
/// the full ceiling, as it did before the timeout was sized at all.
fn repro_timeout(passed: &Run, baseline: Option<&Run>) -> Duration {
    let baseline = match baseline {
        Some(b) if !b.timed_out => b.secs,
        _ => return TEST_TIMEOUT,
    };
    Duration::from_secs_f64(passed.secs.max(baseline) * 3.0)
        .clamp(Duration::from_secs(30), TEST_TIMEOUT)
}

/// `slag fix`: resolve the key and the issue, then fix it under the
/// dashboard or in stream mode.
pub async fn run(
    args: FixArgs,
    tui_flag: bool,
    trace: Option<PathBuf>,
    temper: bool,
) -> Result<(), SlagError> {
    let _provider_run = crate::error::ProviderRunGuard::begin();
    if let Some(m) = &args.model {
        std::env::set_var("SLAG_MODEL_BASE", m);
    }
    if let Some(m) = &args.plan_model {
        std::env::set_var("SLAG_MODEL_PLAN", m);
    }
    let cwd = std::env::current_dir()?;
    let git = Git::open(&cwd);

    // Cheap refusals first: nothing below is worth a key prompt or a model
    // call when there is no issue, or the result cannot land where asked.
    if args.issue_file.is_none() && args.issue.iter().all(|w| w.trim().is_empty()) {
        return Err(SlagError::Config(issue::USAGE.into()));
    }
    if args.sequential && args.evolve == 0 {
        return Err(SlagError::Config("--sequential needs --evolve K".into()));
    }
    if args.pr {
        if git.is_none() {
            return Err(SlagError::Config("--pr needs a git repository".into()));
        }
        if !repo::on_path("gh") {
            return Err(SlagError::Config("--pr needs the GitHub CLI (`gh`) on PATH".into()));
        }
    }

    let config = EngineConfig::resolve().await?;
    if let Some(dir) = crate::cli::sessions_dir() {
        if let Some(other) = crate::cli::conflict_in(&dir, &cwd) {
            return Err(SlagError::Config(format!(
                "another slag (pid {}, started {}) is already working in this directory",
                other.pid, other.started_at
            )));
        }
    }
    let run_id = format!("fix-{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
    let session =
        crate::cli::sessions_dir().and_then(|dir| crate::cli::register_session_in(&dir, &run_id, "fix"));
    if let Some(path) = session.clone() {
        crate::shutdown::register(move || {
            let _ = std::fs::remove_file(&path);
        });
    }
    // A Ctrl-C must not leave a test run writing into the repository.
    crate::shutdown::register(repo::kill_running);

    // Outside git, a shadow repository still tracks what the session
    // changed, so the change list, the test guard, and the reproduction
    // check work the same. It is taken after the refusals above.
    let git = git.or_else(|| Git::shadow(&cwd));
    let job = Job {
        temper: temper || args.temper,
        args,
        cwd,
        git,
    };
    // `--tui` needs a real terminal for the key reader; headless runs keep
    // the stream display, as a forge does.
    let result = if (tui_flag || job.args.tui) && std::io::stdin().is_terminal() {
        under_dashboard(job, &config, trace).await
    } else {
        in_stream(&job, &config, trace).await
    };

    if let Some(path) = session {
        let _ = std::fs::remove_file(path);
    }
    if let Some(error) = crate::error::provider_run_failure() {
        return Err(error);
    }
    let outcome = result?;
    report(&outcome);
    match &outcome.verdict {
        Verdict::Fixed => Ok(()),
        Verdict::Failed(reason) => Err(SlagError::ProofFailed {
            id: FIX_ID.into(),
            reason: reason.clone(),
        }),
    }
}

/// One `slag fix` invocation's fixed inputs.
struct Job {
    args: FixArgs,
    temper: bool,
    cwd: PathBuf,
    git: Option<Git>,
}

async fn in_stream(
    job: &Job,
    config: &EngineConfig,
    trace: Option<PathBuf>,
) -> Result<Outcome, SlagError> {
    let (hooks, sink) = crate::render::trace::attach(EngineHooks::default(), trace);
    let result = fix(job, config, &hooks).await;
    drop(hooks);
    if let Some(sink) = sink {
        let _ = sink.await;
    }
    result
}

/// The dashboard owns the terminal for the run and stays up for review
/// afterwards, as it does for a forge. Text typed at the end is the next
/// issue: the finished fix is reported on the real terminal, and the next
/// one starts from wherever this one left the repository.
async fn under_dashboard(
    mut job: Job,
    config: &EngineConfig,
    trace: Option<PathBuf>,
) -> Result<Outcome, SlagError> {
    let steer = crate::engine::SteerQueue::default();
    let cancel = crate::engine::CancelFlag::default();
    loop {
        let (tx, rx) = crate::engine::events::channel();
        let hooks = EngineHooks {
            events: Some(tx),
            steer: Some(steer.clone()),
            cancel: Some(cancel.clone()),
        };
        let (hooks, trace_sink) = crate::render::trace::attach(hooks, trace.clone());
        tui::set_quiet(true);
        let dash = tokio::spawn(crate::dashboard::run(rx, steer.clone(), cancel.clone()));
        let result = fix(&job, config, &hooks).await;
        if let Err(e) = &result {
            emit(&hooks.events, EngineEvent::Error { message: format!("fix stopped: {e}") });
        }
        drop(hooks);
        let next = dash.await.ok().and_then(|r| r.ok()).flatten();
        if let Some(sink) = trace_sink {
            let _ = sink.await;
        }
        tui::set_quiet(false);
        match (next, result) {
            (Some(next), Ok(outcome)) => {
                report(&outcome);
                job.args.issue = vec![next];
                job.args.issue_file = None;
            }
            (_, result) => return result,
        }
    }
}

/// How the fix ended. Decided only by commands slag ran.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Fixed,
    Failed(String),
}

/// Why an attempt failed: one line for the verdict, the full story for
/// the next attempt's prompt.
struct Failure {
    line: String,
    detail: String,
}

impl Failure {
    fn new(line: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { line: line.into(), detail: detail.into() }
    }
}

/// `1 attempt`, `3 attempts`.
fn plural(n: u8, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// How the bug was shown to exist, if it was.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Repro {
    /// With the source change reverted, the session's new test fails.
    Regression { status: String },
    /// With the source change reverted, the check never finishes: for a
    /// hang bug, that IS the reproduction.
    Hangs { status: String },
    /// The test command failed before any edit and passes now.
    Baseline { status: String },
    NotShown(String),
}

impl Repro {
    /// The new test pins the bug: it fails or hangs without the fix.
    fn caught(&self) -> bool {
        matches!(self, Repro::Regression { .. } | Repro::Hangs { .. })
    }
}

/// Everything the final report needs.
struct Outcome {
    issue: Issue,
    verdict: Verdict,
    attempts: u8,
    checks: Vec<Run>,
    repro: Repro,
    stat: Vec<String>,
    summary: String,
    branch: Option<String>,
    commit: Option<String>,
    pr: Option<String>,
    notes: Vec<String>,
    /// Exactly the paths the fix commits: what the session created or
    /// changed, never a file the user had dirty before the run.
    files: Vec<String>,
    /// Where the wall time went, phase by phase, so waste is visible.
    phases: Phases,
    /// Run time and spend, taken when the fix ends rather than when the
    /// report prints: a dashboard left open must not inflate the wall.
    time: Option<String>,
    spend: Option<String>,
    /// What `--evolve` did, stage by stage. `None` on the fast path.
    evolve: Option<evolve::Summary>,
}

/// Seconds per phase of one fix, summed across attempts.
#[derive(Debug, Default, Clone, Copy)]
struct Phases {
    gather: f64,
    evolve: f64,
    session: f64,
    verify: f64,
    repro: f64,
    review: f64,
    land: f64,
}

impl Phases {
    /// The phases that took measurable time. A step done in microseconds
    /// (a review slag settles from the diff, a cached verify) printed as
    /// "0.0s" beside its own verdict reads as a step that never ran.
    fn line(&self) -> String {
        [
            ("gather", self.gather),
            ("evolve", self.evolve),
            ("session", self.session),
            ("verify", self.verify),
            ("reproduce", self.repro),
            ("review", self.review),
            ("commit", self.land),
        ]
        .iter()
        .filter(|(_, secs)| *secs >= 0.05)
        .map(|(name, secs)| format!("{name} {secs:.1}s"))
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

/// Before the first model call: the issue, the test command, and how the
/// tests end with nothing changed, gathered concurrently.
async fn gather(job: &Job) -> Result<(Issue, Option<TestCmd>, Option<Run>), SlagError> {
    let test = match &job.args.test {
        Some(cmd) => Some(TestCmd { cmd: cmd.clone(), source: "--test".into() }),
        None => repo::detect_test(&job.cwd),
    };
    let baseline = async {
        match &test {
            Some(t) => Some(repo::run(&t.cmd, &job.cwd, TEST_TIMEOUT).await),
            None => None,
        }
    };
    let (issue, baseline) =
        tokio::join!(issue::resolve(&job.args.issue, job.args.issue_file.as_deref()), baseline);
    Ok((issue?, test, baseline))
}

async fn fix(job: &Job, config: &EngineConfig, hooks: &EngineHooks) -> Result<Outcome, SlagError> {
    stats::mark_run_start();
    tui::show_banner();
    tui::header("FIX");

    let mut phases = Phases::default();
    let clock = std::time::Instant::now();
    let (issue, test, baseline) = gather(job).await?;
    phases.gather = clock.elapsed().as_secs_f64();
    emit(
        &hooks.events,
        EngineEvent::IngotStart { id: FIX_ID.into(), work: issue.title.clone() },
    );
    tui::status_line("░", tui::COLD, &format!("issue: {}", issue_line(&issue)));

    // A test command that cannot run here says nothing about the bug, and
    // verifying with it would fail every attempt for the wrong reason.
    let usable_test = match (&test, &baseline) {
        (Some(t), Some(b)) if !b.unusable() => Some(t.cmd.clone()),
        _ => None,
    };
    match (&test, &baseline) {
        (Some(t), Some(b)) => {
            let line =
                format!("tests: `{}` (from {}) {} before any edit", t.cmd, t.source, b.status());
            note(hooks, &line, b.ok());
        }
        _ => note(hooks, "tests: none detected; the session must find them", false),
    }

    if let Some(git) = &job.git {
        git.hide_scratch();
    }
    let before = job.git.as_ref().map(Git::snapshot).unwrap_or_default();
    let files = repo::file_list(&job.cwd, job.git.as_ref());
    let test_files: Vec<String> = files.iter().filter(|f| repo::is_test_path(f)).cloned().collect();
    // The issue's own command, run as the user ran it, so the first test can
    // reproduce exactly that and not a call the session imagined.
    let mut reported = String::new();
    for cmd in repo::issue_commands(&issue.render(), &job.cwd) {
        let run = repo::run(&cmd, &job.cwd, ISSUE_COMMAND_TIMEOUT).await;
        note(hooks, &format!("issue command: `{cmd}` {} before any edit", run.status()), true);
        reported.push_str(&format!(
            "`{cmd}` gave {}. The end of its output:\n{}\n",
            run.status(),
            repo::tail(&run.output, 20, 2_000)
        ));
    }
    let mut context =
        context_block(job, &issue, &files, &test_files, test.as_ref(), baseline.as_ref());
    if !reported.is_empty() {
        context.push_str(&format!(
            "\n=== THE ISSUE'S COMMAND, RUN BEFORE ANY EDIT ===\n{reported}Your first test \
             reproduces what this command does: the same call, input, and defaults.\n"
        ));
    }
    let rescuer = Rescuer { register: &register_shutdown, stop: repo::kill_running };
    let mut ctx = Ctx {
        job,
        config,
        hooks,
        issue: &issue,
        context: &context,
        before: &before,
        usable_test: usable_test.as_deref(),
        rescuer: &rescuer,
        smith: SmithOpts::default(),
    };
    let tries = job.args.tries.max(1);

    // The execution harness (`crate::harness`): `--harness DIR`, the
    // library's entries for this domain, or none at all. The fast path
    // asks for none and pays for none of this.
    let harnessed = job.args.harness.is_some() || job.args.evolve > 0;
    let domain = match (&job.args.domain, harnessed) {
        (Some(d), _) => d.clone(),
        (None, true) => {
            let top = job.git.as_ref().map_or(job.cwd.as_path(), |g| g.top.as_path());
            crate::harness::library::domain_of(top, test.as_ref().map(|t| t.cmd.as_str()))
        }
        (None, false) => String::new(),
    };
    let library = harnessed
        .then(|| crate::harness::library::Library::resolve(job.args.library.as_deref(), &job.cwd))
        .flatten();
    let (base, harness_line) = base_harness(job, &issue, &domain, library.as_ref(), hooks).await?;
    ctx.smith.harness = base.as_deref().map(|d| Arc::new(crate::harness::Harness::load(d)));

    let mut failure: Option<Failure> = None;
    let mut checks: Vec<Run> = Vec::new();
    let mut summary = String::new();
    let mut changed: Vec<String> = Vec::new();
    let mut declared: Option<String> = None;
    let mut passed = false;
    let mut attempts = 0u8;
    let mut first = 1u8;

    // `--evolve K` takes the first attempt's place: the recursion runs in
    // copies of the repository, and the fix it delivers lands here only to
    // face the same checks as any attempt.
    let mut evolved = None;
    if job.args.evolve > 0 {
        attempts = 1;
        first = 2;
        emit(&hooks.events, EngineEvent::HeatTick { id: FIX_ID.into(), heat: 1 });
        let settings = evolve::Settings::resolve(job, config, domain.clone(), library.clone(), base.clone());
        let clock = std::time::Instant::now();
        let ev = evolve::run(&ctx, baseline.as_ref(), settings).await?;
        phases.evolve += clock.elapsed().as_secs_f64();
        ctx.smith.harness = Some(Arc::new(crate::harness::Harness::load(&ev.adopted())));
        match ev.deliver(job, &before) {
            Err(why) => {
                note(hooks, &format!("evolve: {why}"), false);
                failure = Some(Failure::new(why.clone(), why));
            }
            Ok(Some(response)) => {
                let label = format!("evolved fix ({})", ev.chosen_label().unwrap_or_default());
                match ctx.check(&label, &response, &mut phases).await? {
                    Attempted::Passed { checks: c, summary: s, changed: ch, declared: d } => {
                        (checks, summary, changed, declared) = (c, s, ch, d);
                        passed = true;
                    }
                    Attempted::Failed { failure: f, summary: s, changed: ch } => {
                        (failure, summary, changed) = (Some(f), s, ch);
                    }
                }
            }
            Ok(None) => failure = ev.failure().map(|(line, detail)| Failure::new(line, detail)),
        }
        evolved = Some(ev);
    }

    for attempt in first..=tries {
        if passed {
            break;
        }
        attempts = attempt;
        emit(&hooks.events, EngineEvent::HeatTick { id: FIX_ID.into(), heat: attempt });
        // The first attempt runs on the work model. Anything that needs a
        // second look gets the plan model and the failure in hand.
        let grade = if attempt == 1 { 1 } else { HIGH_GRADE };
        tui::status_line(
            "⚒",
            tui::HOT,
            &format!("attempt {attempt}/{tries} · {}", config.model_for_grade(grade)),
        );
        let retry = failure.as_ref().map(|f| retry_block(&f.detail, &ctx.diff(&changed)));
        let label = format!("attempt {attempt}");
        match ctx.attempt(grade, retry.as_deref(), &label, &mut phases).await? {
            Attempted::Passed { checks: c, summary: s, changed: ch, declared: d } => {
                (checks, summary, changed, declared) = (c, s, ch, d);
                passed = true;
                break;
            }
            Attempted::Failed { failure: f, summary: s, changed: ch } => {
                (failure, summary, changed) = (Some(f), s, ch);
            }
        }
    }

    let mut out = Outcome {
        issue: issue.clone(),
        verdict: Verdict::Fixed,
        attempts,
        checks,
        repro: Repro::NotShown("the fix did not pass".into()),
        stat: Vec::new(),
        summary,
        branch: None,
        commit: None,
        pr: None,
        notes: Vec::new(),
        files: Vec::new(),
        phases,
        time: None,
        spend: None,
        evolve: None,
    };
    out.notes.extend(harness_line);
    if let Some(git) = &job.git {
        out.stat = git.stat(&changed);
        let reverted = git.reverted(&before);
        if !reverted.is_empty() {
            let line = format!(
                "the session reset uncommitted work in {} to HEAD",
                reverted.join(", ")
            );
            note(hooks, &line, false);
            out.notes.push(line);
        }
    }

    if !passed {
        let line = failure.map_or_else(|| "no attempt ran".to_string(), |f| f.line);
        emit(&hooks.events, EngineEvent::IngotDone { id: FIX_ID.into(), ok: false });
        out.verdict = Verdict::Failed(format!("{line} after {}", plural(attempts, "attempt")));
        if !changed.is_empty() {
            out.notes.push("the last attempt's changes are left in the working tree".into());
        }
        reflect(&ctx, evolved, &mut out).await;
        stamp(&mut out);
        return Ok(out);
    }

    if let Some(git) = &job.git {
        sweep_debris(git, &before, &mut changed, &mut out.notes, hooks);
    }
    let edited_now = |changed: &[String]| -> Vec<EditedTest> {
        job.git.as_ref().map(|g| edited_old_tests(g, &before, changed)).unwrap_or_default()
    };
    let paths = |edited: &[EditedTest]| -> Vec<String> {
        edited.iter().map(|e| e.path.clone()).collect()
    };
    let edited = edited_now(&changed);
    let clock = std::time::Instant::now();
    let proof_cmd = out.checks[0].cmd.clone();
    let (mut repro, mut without) = reproduce(
        job,
        &before,
        &changed,
        &paths(&edited),
        &proof_cmd,
        repro_timeout(&out.checks[0], baseline.as_ref()),
        baseline.as_ref(),
        &rescuer,
    )
    .await;
    out.phases.repro += clock.elapsed().as_secs_f64();
    note_repro(hooks, &repro, &proof_cmd);

    // A green fix can still lose to a careful human: a symptom patch, a
    // test that does not encode the reported case, a test in the wrong
    // file, a test that cannot run here. One gap, one chance to close it.
    let clock = std::time::Instant::now();
    let found = ctx
        .quality_gap(
            &changed,
            &test_files,
            &repro,
            without.as_ref(),
            &edited,
            declared.as_deref(),
            &proof_cmd,
        )
        .await?;
    let mut gap_open = found.as_ref().map(|g| g.text.clone());
    out.phases.review += clock.elapsed().as_secs_f64();
    if let (Some(found), Some(git)) = (found, job.git.as_ref()) {
        let gap = found.text;
        note(hooks, &format!("review: {gap}"), false);
        let kept = keep(git, &changed);
        // What the first fix proved, so a revision can only add to it.
        let first_added = added_test(git, &before, &changed, &edited);
        let first_caught = repro.caught();
        let first_tests: Vec<(PathBuf, repo::Content)> = changed
            .iter()
            .zip(&kept)
            .filter(|(p, _)| repo::is_test_path(p) && !edited.iter().any(|e| &e.path == *p))
            .map(|(_, (path, content, _))| (path.clone(), content.clone()))
            .collect();
        let block = revision_block(&gap, &ctx.diff(&changed));
        emit(&hooks.events, EngineEvent::HeatTick { id: FIX_ID.into(), heat: attempts + 1 });
        tui::status_line(
            "⚒",
            tui::HOT,
            &format!("revision · {}", config.model_for_grade(HIGH_GRADE)),
        );
        match ctx.attempt(HIGH_GRADE, Some(&block), "revision", &mut out.phases).await? {
            // A revision that passes by changing nothing closed nothing.
            Attempted::Passed { changed: ch, .. } if unchanged(git, &kept, &ch) => {
                note(hooks, "revision changed nothing; kept the first fix", false);
                out.notes.push(format!("review gap left open: {gap}"));
            }
            Attempted::Passed { checks: c, summary: s, changed: mut ch, declared: d } => {
                sweep_debris(git, &before, &mut ch, &mut out.notes, hooks);
                let ed = edited_now(&ch);
                let proof_cmd = c[0].cmd.clone();
                let clock = std::time::Instant::now();
                let (r, w) = reproduce(
                    job,
                    &before,
                    &ch,
                    &paths(&ed),
                    &proof_cmd,
                    repro_timeout(&c[0], baseline.as_ref()),
                    baseline.as_ref(),
                    &rescuer,
                )
                .await;
                // A reviewer can be wrong, and a revision that follows a
                // wrong gap makes the fix worse. It may add to what the
                // first fix proved, never take from it.
                let mut worse = None;
                // A gap slag measured is closed only when the measure says so.
                let closes = match found.kind {
                    GapKind::NoTest => added_test(git, &before, &ch, &ed),
                    GapKind::PassesWithoutFix => !w.as_ref().is_some_and(|run| run.ok()),
                    GapKind::Orphan => ctx.orphans(&ch, &proof_cmd).await.is_empty(),
                    GapKind::Review => true,
                };
                if !closes {
                    worse = Some("did not close the gap");
                } else if first_added && !added_test(git, &before, &ch, &ed) {
                    worse = Some("dropped the regression test");
                } else if first_caught && !r.caught() {
                    worse = Some("left tests that no longer catch the bug");
                } else if found.kind == GapKind::Review && !first_tests.is_empty() {
                    // The first fix's tests encode the issue; against the
                    // revised source they must still pass.
                    let run = swap_and_run(job, first_tests.clone(), &proof_cmd, TEST_TIMEOUT, &rescuer).await;
                    if run.is_some_and(|run| !run.ok()) {
                        worse = Some("broke what the first fix's tests pin");
                    }
                }
                out.phases.repro += clock.elapsed().as_secs_f64();
                if let Some(why) = worse {
                    let now = git.changed_since(&before);
                    roll_back(git, &before, &kept, &now);
                    note(hooks, &format!("kept the first fix; the revision {why}"), false);
                    out.notes.push(format!("review gap left open: {gap}"));
                } else {
                    (out.checks, out.summary, changed, declared) = (c, s, ch, d);
                    (repro, without) = (r, w);
                    note_repro(hooks, &repro, &proof_cmd);
                    out.notes.push(format!("revised once after review: {gap}"));
                    gap_open = None;
                }
            }
            Attempted::Failed { failure, changed: now, .. } => {
                // The first fix passed; a revision that broke it must not
                // replace it. Put the first fix back exactly.
                roll_back(git, &before, &kept, &now);
                note(
                    hooks,
                    &format!("kept the first fix; the revision did not pass: {}", failure.line),
                    false,
                );
                out.notes.push(format!("review gap left open: {gap}"));
            }
        }
        out.stat = git.stat(&changed);
    }
    // A change to what existing tests pin stands only when the issue asks
    // for it and a review of the final change explicitly agrees. A gap left
    // open, a skipped review, or a garbled one: not a fix to hand over.
    if let Some(said) = &declared {
        let agreed = match &gap_open {
            Some(gap) => Err(gap.clone()),
            None => {
                let edited = edited_now(&changed);
                ctx.declared_agreed(&changed, &test_files, &repro, without.as_ref(), &edited, said)
                    .await?
            }
        };
        if let Err(why) = agreed {
            emit(&hooks.events, EngineEvent::IngotDone { id: FIX_ID.into(), ok: false });
            out.verdict = Verdict::Failed(format!(
                "changes existing tests without the review agreeing the issue asks for it \
                 ({said}): {why}"
            ));
            out.notes.push("the changes are left in the working tree".into());
            out.repro = repro;
            stamp(&mut out);
            return Ok(out);
        }
    }
    // Tests that pass with and without the source change, after the one
    // chance to fix them, are no evidence the change does anything; with no
    // failing test before the edit either, nothing says the bug is gone.
    let unproven =
        without.as_ref().is_some_and(|run| run.ok()) && !matches!(repro, Repro::Baseline { .. });
    drop(without);
    out.repro = repro;
    if unproven {
        emit(&hooks.events, EngineEvent::IngotDone { id: FIX_ID.into(), ok: false });
        out.verdict = Verdict::Failed(
            "no test shows the change fixes anything: the new tests pass with the source \
             change reverted"
                .into(),
        );
        out.notes.push("the changes are left in the working tree".into());
        stamp(&mut out);
        return Ok(out);
    }
    if let Some(said) = &declared {
        note(hooks, said, false);
        out.notes.push(said.clone());
    }
    emit(&hooks.events, EngineEvent::IngotDone { id: FIX_ID.into(), ok: true });

    let clock = std::time::Instant::now();
    land(job, &mut out, &changed, &before);
    out.phases.land = clock.elapsed().as_secs_f64();
    reflect(&ctx, evolved, &mut out).await;
    stamp(&mut out);
    Ok(out)
}

/// The base harness H0, as a directory, when there is one: `--harness
/// DIR` (validated in a copy, so the user's tree is never pruned), or for
/// `--harness library` a tree the library builds for this issue with no
/// model call. A tree that fails validation is dropped with a note.
async fn base_harness(
    job: &Job,
    issue: &Issue,
    domain: &str,
    library: Option<&crate::harness::library::Library>,
    hooks: &EngineHooks,
) -> Result<(Option<PathBuf>, Option<String>), SlagError> {
    let spec = job.args.harness.as_deref().map(str::trim).unwrap_or_default();
    let scratch = job.cwd.join(".slag").join("harness");
    let from;
    let dir = match spec {
        "" | "none" => return Ok((None, None)),
        "library" => {
            let Some(lib) = library else {
                note(hooks, "harness: no library is configured; running without one", false);
                return Ok((None, None));
            };
            let dst = scratch.join("library");
            let repository = job
                .git
                .as_ref()
                .and_then(|g| g.top.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let used = lib.apply(domain, &issue.render(), &repository, &dst);
            if used.is_empty() {
                note(hooks, &format!("harness: no {domain} library entry applies here; running without one"), false);
                return Ok((None, None));
            }
            let entries: Vec<String> =
                used.iter().map(|(id, parts)| format!("{id} ({})", parts.join(", "))).collect();
            let line = format!("harness from the {domain} library: {}", entries.join("; "));
            note(hooks, &line, true);
            from = Some(line);
            dst
        }
        path => {
            let given = PathBuf::from(path);
            if !given.is_dir() {
                return Err(SlagError::Config(format!("--harness {path}: no such directory")));
            }
            let dst = scratch.join("given");
            crate::harness::copy_tree(&given, &dst)?;
            from = None;
            dst
        }
    };
    match crate::harness::validate::validate(&dir).await {
        Ok(_) => {
            let loaded = crate::harness::Harness::load(&dir);
            note(hooks, &format!("harness: {}", loaded.summary()), true);
            let from = from.unwrap_or_else(|| format!("harness from {spec}: {}", loaded.summary()));
            Ok(if loaded.is_empty() { (None, None) } else { (Some(dir), Some(from)) })
        }
        Err(why) => {
            let first = why.lines().find_map(|l| l.trim().strip_prefix("- ")).unwrap_or(why.as_str());
            let line = format!("harness dropped, it failed validation: {first}");
            note(hooks, &line, false);
            Ok((None, Some(line)))
        }
    }
}

/// After an evolved fix: reflect, update the library, and put the stage
/// table on the report.
async fn reflect(ctx: &Ctx<'_>, evolved: Option<evolve::Evolved>, out: &mut Outcome) {
    let Some(mut ev) = evolved else { return };
    let clock = std::time::Instant::now();
    let verdict = match &out.verdict {
        Verdict::Fixed => match out.checks.first() {
            Some(c) => format!("fixed, `{}` {}", c.cmd, c.status()),
            None => "fixed".to_string(),
        },
        Verdict::Failed(why) => format!("not fixed: {why}"),
    };
    ev.reflect(ctx, &verdict).await;
    out.phases.evolve += clock.elapsed().as_secs_f64();
    out.evolve = Some(ev.summary());
}

fn note_repro(hooks: &EngineHooks, repro: &Repro, cmd: &str) {
    match repro {
        Repro::NotShown(why) => note(hooks, &format!("reproduction not shown: {why}"), false),
        Repro::Regression { status } => {
            note(hooks, &format!("reproduced: without the fix `{cmd}` gives {status}"), true)
        }
        Repro::Hangs { status } => {
            note(hooks, &format!("reproduced: without the fix `{cmd}` hangs ({status})"), true)
        }
        Repro::Baseline { status } => {
            note(hooks, &format!("reproduced: the tests gave {status} before the fix"), true)
        }
    }
}

/// What every session of one fix shares: the job, the issue, and the
/// repository as it was before the first edit.
struct Ctx<'a> {
    job: &'a Job,
    config: &'a EngineConfig,
    hooks: &'a EngineHooks,
    issue: &'a Issue,
    context: &'a str,
    before: &'a BTreeMap<String, repo::Content>,
    usable_test: Option<&'a str>,
    rescuer: &'a Rescuer<'a>,
    smith: SmithOpts,
}

/// How a fix session's smith is built beyond the model: the harness it
/// runs under and, inside `--evolve`, where its transcript goes and which
/// budget it spends. The default is the plain fast path.
#[derive(Clone, Default)]
struct SmithOpts {
    harness: Option<Arc<crate::harness::Harness>>,
    transcripts: Option<PathBuf>,
    budget: Option<crate::engine::agent::SharedBudget>,
}

/// One session, judged by slag.
enum Attempted {
    /// The change passed every check slag ran. `declared` is the session's
    /// own account of a deliberate change to existing tests, when it made
    /// one.
    Passed { checks: Vec<Run>, summary: String, changed: Vec<String>, declared: Option<String> },
    /// It did not. `changed` is what the session left on disk.
    Failed { failure: Failure, summary: String, changed: Vec<String> },
}

impl Ctx<'_> {
    fn changed_now(&self) -> Vec<String> {
        self.job.git.as_ref().map(|g| g.changed_since(self.before)).unwrap_or_default()
    }

    /// The session's change against the code before the fix, as one
    /// unified diff. New files show in full, which `git diff` cannot do
    /// without staging them.
    fn diff(&self, changed: &[String]) -> String {
        let Some(git) = &self.job.git else { return String::new() };
        let mut out = String::new();
        for p in changed {
            let old = git.original(self.before, p).map(|b| String::from_utf8_lossy(&b).into_owned());
            let new = std::fs::read(git.top.join(p)).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
            let (a, b) = (old.as_deref().unwrap_or(""), new.as_deref().unwrap_or(""));
            let from = if old.is_some() { format!("a/{p}") } else { "/dev/null".to_string() };
            let to = if new.is_some() { format!("b/{p}") } else { "/dev/null".to_string() };
            out.push_str(
                &similar::TextDiff::from_lines(a, b).unified_diff().header(&from, &to).to_string(),
            );
        }
        out
    }

    /// One fixing session, then slag's own checks: something changed, no
    /// existing test was lost, the tests pass, and with `--temper` the
    /// warden agrees.
    async fn attempt(
        &self,
        grade: u8,
        retry: Option<&str>,
        label: &str,
        phases: &mut Phases,
    ) -> Result<Attempted, SlagError> {
        let (job, hooks) = (self.job, self.hooks);
        let prompt = crate::flux::fix_prompt(&self.issue.render(), self.context, retry);
        // `job.cwd` is the directory `make_smith` would root at; an evolve
        // run points it at a copy of the repository instead.
        let mut smith = crate::smith::native::NativeSmith::forge(
            self.config.clone(),
            "default",
            grade,
            job.cwd.clone(),
            hooks,
        )
        .with_harness(self.smith.harness.clone())
        .with_transcript_root(self.smith.transcripts.clone());
        if let Some(budget) = &self.smith.budget {
            smith = smith.with_shared_budget(Some(budget.clone()));
        }
        let smith: Box<dyn crate::smith::Smith> = Box::new(smith);
        let clock = std::time::Instant::now();
        let response = super::forge::invoke_absorbing_transients(
            smith.as_ref(),
            &prompt,
            FIX_ID,
            super::forge::MAX_TRANSIENT_PER_HEAT,
            hooks,
        )
        .await;
        phases.session += clock.elapsed().as_secs_f64();
        let fail = |failure: Failure, summary: String| {
            Ok(Attempted::Failed { failure, summary, changed: self.changed_now() })
        };
        let response = match response {
            Ok(r) => r,
            Err(e @ (SlagError::Cancelled | SlagError::RunBudgetExhausted { .. })) => return Err(e),
            Err(e) if e.is_deterministic_provider() => return Err(e),
            Err(e) => {
                note(hooks, &format!("{label}: session error: {e}"), false);
                return fail(
                    Failure::new(
                        format!("the session failed: {e}"),
                        format!("The session ended with an error: {e}"),
                    ),
                    String::new(),
                );
            }
        };
        self.check(label, &response, phases).await
    }

    /// slag's own checks on what a session left on disk: something
    /// changed, no existing test was lost, the tests pass, and with
    /// `--temper` the warden agrees. `response` is the session's finish
    /// text, which carries its `CMD:` line.
    async fn check(&self, label: &str, response: &str, phases: &mut Phases) -> Result<Attempted, SlagError> {
        let (job, hooks) = (self.job, self.hooks);
        let fail = |failure: Failure, summary: String| {
            Ok(Attempted::Failed { failure, summary, changed: self.changed_now() })
        };
        let agent_cmd = crate::proof::extract_cmd(response);
        let summary = strip_cmd_lines(response);
        let changed = self.changed_now();

        if let Some(git) = &job.git {
            if changed.is_empty() {
                // Edits to the user's own uncommitted files never count:
                // slag will not commit them, so they cannot be the fix.
                let yours = git.yours_changed(self.before);
                let detail = if yours.is_empty() {
                    "The session finished without changing any file.".to_string()
                } else {
                    format!(
                        "The session changed only files that already had uncommitted \
                         changes before the run ({}). Those belong to the user and slag \
                         never commits them. Fix the bug in files that were clean.",
                        yours.join(", ")
                    )
                };
                note(hooks, &format!("{label}: no file changed"), false);
                return fail(Failure::new("the session changed no file", detail), summary);
            }
            // A suite goes green easily when tests disappear. The rule is
            // in the prompt; this is the check that it held.
            if let Some(lost) = lost_tests(git, self.before, &changed) {
                note(hooks, &format!("{label}: {lost}"), false);
                // Told to restore, a weak model removes the same test again.
                // Put the old tests back here, and show exactly what was lost.
                let edited = edited_old_tests(git, self.before, &changed);
                let olds: Vec<Saved> = edited
                    .iter()
                    .map(|e| (git.top.join(&e.path), e.original_tests.clone(), None))
                    .collect();
                restore(&olds);
                let names: Vec<&str> = edited.iter().map(|e| e.path.as_str()).collect();
                return fail(
                    Failure::new(
                        lost.clone(),
                        format!(
                            "The session {lost}. slag put {} back as it was, with every \
                             existing test whole and unskipped; any new tests you had put \
                             there are gone with the edit. Add them again after the last \
                             existing test, or in a new test file, and change none of these \
                             lines:\n{}",
                            names.join(", "),
                            removed_text(&edited)
                        ),
                    ),
                    summary,
                );
            }
        }

        // A test alone fixes nothing. A session that touched only tests
        // most often read the reported behavior as the intended one.
        if job.git.is_some() && changed.iter().all(|p| repo::is_test_path(p)) {
            note(hooks, &format!("{label}: only tests changed"), false);
            return fail(
                Failure::new(
                    "only tests changed; the code was not fixed",
                    "The session changed only test files. The issue reports wrong behavior \
                     in the code, and a fix changes the code. Re-read the issue: the behavior \
                     it reports is the bug, and your test must expect what the issue says \
                     should happen instead.",
                ),
                summary,
            );
        }

        // The session's own test runs cached bytecode for whatever was on
        // disk then; slag's verdict must come from the source as it is.
        if let Some(git) = &job.git {
            for p in &changed {
                repo::purge_bytecode(&git.top.join(p));
            }
        }
        let clock = std::time::Instant::now();
        let checks = verify(job, hooks, self.usable_test, agent_cmd.as_deref()).await;
        phases.verify += clock.elapsed().as_secs_f64();
        if checks.is_empty() {
            let line = "nothing to verify with: no test command found and no finish `cmd`";
            note(hooks, &format!("{label}: {line}"), false);
            return fail(
                Failure::new(
                    line,
                    "slag had nothing to verify with: no test command was detected and the \
                     finish call carried no `cmd`. Find how this repository runs its tests \
                     and put that command in `cmd`.",
                ),
                summary,
            );
        }
        if let Some(bad) = checks.iter().find(|c| !c.ok()) {
            return fail(
                Failure::new(
                    format!("`{}` gave {}", bad.cmd, bad.status()),
                    format!(
                        "After the session finished, slag ran `{}` and got {}:\n{}",
                        bad.cmd,
                        bad.status(),
                        repo::tail(&bad.output, OUTPUT_TAIL_LINES, OUTPUT_TAIL_BYTES)
                    ),
                ),
                summary,
            );
        }

        // Existing tests are the user's contract. Run them as they were
        // against the new source: an edit that hides a broken expectation
        // fails here, before any model gets a say.
        let mut declared = None;
        if let Some(git) = &job.git {
            let edited = edited_old_tests(git, self.before, &changed);
            if !edited.is_empty() {
                let names: Vec<&str> = edited.iter().map(|e| e.path.as_str()).collect();
                let names = names.join(", ");
                let swaps = edited
                    .iter()
                    .map(|e| (git.top.join(&e.path), e.original_tests.clone()))
                    .collect();
                let clock = std::time::Instant::now();
                let run = swap_and_run(job, swaps, &checks[0].cmd, TEST_TIMEOUT, self.rescuer).await;
                phases.verify += clock.elapsed().as_secs_f64();
                let Some(run) = run else { return Err(SlagError::Cancelled) };
                let removed = removed_text(&edited);
                // Whatever happens next, the retry starts from the old tests
                // as they were, beside the session's source change: a model
                // told to restore lines often cannot, but it can add.
                let put_back = || {
                    let olds: Vec<Saved> = edited
                        .iter()
                        .map(|e| (git.top.join(&e.path), e.original_tests.clone(), None))
                        .collect();
                    restore(&olds);
                };
                if run.ok() {
                    note(hooks, &format!("{label}: edited existing tests that pass unchanged: {names}"), false);
                    put_back();
                    // A model that replaced a test while meaning to add one
                    // tends to do it again; a new file cannot break an old one.
                    let fresh: Vec<String> = edited
                        .iter()
                        .filter(|e| repo::is_test_path(&e.path))
                        .map(|e| repo::regression_sibling(&e.path))
                        .collect();
                    let instead = match fresh.first() {
                        Some(path) => format!("Put your new tests in a new file instead, such as {path}"),
                        None => "Add your new tests after the last existing test".to_string(),
                    };
                    return fail(
                        Failure::new(
                            format!("edited existing tests that pass unchanged: {names}"),
                            format!(
                                "You deleted or rewrote lines of existing tests, though the \
                                 original tests pass against your fix. slag put {names} back \
                                 as it was, so any new tests you had added there are gone with \
                                 the edit. Do not edit {names} again. {instead}, and leave \
                                 every existing line alone. The lines you had changed:\n{removed}"
                            ),
                        ),
                        summary,
                    );
                }
                declared = declaration(&summary, &edited);
                if declared.is_none() {
                    note(hooks, &format!("{label}: the fix breaks existing tests: {names}"), false);
                    put_back();
                    return fail(
                        Failure::new(
                            format!("the fix breaks existing tests in {names}"),
                            format!(
                                "Your fix breaks existing tests, and you edited them to pass. \
                                 slag ran the original {names} against your fix: `{}` gave \
                                 {}:\n{}\n\nslag put {names} back as it was. The lines you \
                                 had changed:\n{removed}\nFix the source so the existing tests \
                                 pass as written, and add your new tests beside them. Only if the \
                                 issue explicitly asks for the behavior they pin, change \
                                 them again and say so in the summary, on its own line: \
                                 changed existing tests: <file>: <why>",
                                run.cmd,
                                run.status(),
                                repo::tail(&run.output, OUTPUT_TAIL_LINES, OUTPUT_TAIL_BYTES)
                            ),
                        ),
                        summary,
                    );
                }
                // The issue has to name what the old expectation pinned: a
                // declaration whose removed lines share nothing with the
                // issue is a rewrite dressed as a request.
                if !ties_to_issue(&edited, &self.issue.render()) {
                    note(hooks, &format!("{label}: declared test change the issue never names"), false);
                    put_back();
                    return fail(
                        Failure::new(
                            format!("changed existing tests the issue never mentions: {names}"),
                            format!(
                                "You changed existing tests and declared it, but nothing in the \
                                 lines you changed appears in the issue, so the issue does not \
                                 ask for that behavior. slag put {names} back. Fix the source so \
                                 they pass as written. The lines you had changed:\n{removed}"
                            ),
                        ),
                        summary,
                    );
                }
                note(hooks, &format!("{label}: changed existing tests, declared"), false);
            }
        }

        if job.temper {
            match judge(self.config, hooks, self.issue, &checks[0].cmd).await {
                Ok(v) if v.fulfilled => {
                    note(hooks, &format!("warden: pass ({})", v.evidence), true)
                }
                Ok(v) => {
                    note(hooks, &format!("warden: {}", v.gap), false);
                    return fail(
                        Failure::new(
                            format!("the warden rejected the fix: {}", v.gap),
                            format!(
                                "The tests pass, but an independent reviewer rejected the \
                                 fix: {}",
                                v.gap
                            ),
                        ),
                        summary,
                    );
                }
                Err(e) if e.is_deterministic_provider() => return Err(e),
                // A warden that cannot run must not sink a verified fix.
                Err(e) => note(hooks, &format!("warden could not judge the fix: {e}"), false),
            }
        }
        Ok(Attempted::Passed { checks, summary, changed, declared })
    }

    /// The one gap worth a revision, if any. Two are certain and cost no
    /// model call: no test was added, or the new test passes without the
    /// fix. Past those, one tool-free review call on a fresh context reads
    /// the issue, the diff, and what the tests did with the fix reverted.
    #[allow(clippy::too_many_arguments)]
    async fn quality_gap(
        &self,
        changed: &[String],
        test_files: &[String],
        repro: &Repro,
        without: Option<&Run>,
        edited: &[EditedTest],
        declared: Option<&str>,
        cmd: &str,
    ) -> Result<Option<Gap>, SlagError> {
        let Some(git) = &self.job.git else { return Ok(None) };
        let has_tests = !test_files.is_empty() || self.usable_test.is_some();
        if !added_test(git, self.before, changed, edited) && has_tests {
            return Ok(Some(Gap::new(
                GapKind::NoTest,
                "no regression test was added: add one that asserts the issue's reported \
                 cases, in the test file for the module you fixed, or in a new file named \
                 after it",
            )));
        }
        // A test file the runner never executes passes with or without the
        // fix, and proves nothing either way. Say why before saying that.
        let orphans = self.orphans(changed, cmd).await;
        if !orphans.is_empty() {
            let wiring = repo::runner_wiring(&self.job.cwd, cmd)
                .map(|w| format!(" How the runner finds tests: {w}"))
                .unwrap_or_default();
            return Ok(Some(Gap::new(
                GapKind::Orphan,
                &format!(
                    "{} never runs: `{cmd}` still passes when it cannot run.{wiring}\nPut the \
                     new tests where the runner already looks: extend an existing test file, \
                     or wire the new file into the runner the way the existing tests are.",
                    orphans.join(", ")
                ),
            )));
        }
        // Checked on the run itself, not the verdict: a failing baseline
        // proves the bug, and says nothing about the new test.
        if without.is_some_and(|run| run.ok()) {
            return Ok(Some(Gap::new(
                GapKind::PassesWithoutFix,
                "the new test also passes with the source change reverted, so it does not \
                 catch the bug: make it assert the exact result the issue reports",
            )));
        }

        let prompt = self.review_prompt(changed, test_files, repro, without, edited, declared, true);
        match review(self.config, self.hooks, self.smith.budget.as_ref(), &prompt).await {
            Ok(Review::Gap(gap)) => Ok(Some(Gap { text: gap, kind: GapKind::Review })),
            Ok(_) => {
                note(self.hooks, "review: pass", true);
                Ok(None)
            }
            Err(e) if e.is_deterministic_provider() => Err(e),
            // An advisory call that cannot run must not sink a verified fix.
            Err(e) => {
                note(self.hooks, &format!("review skipped: {e}"), false);
                Ok(None)
            }
        }
    }

    /// New test files the repository's own test command never executes:
    /// with each made unable to run, the command still passes. One run per
    /// file, for the first three; data files a test reads are skipped.
    async fn orphans(&self, changed: &[String], cmd: &str) -> Vec<String> {
        let Some(git) = &self.job.git else { return Vec::new() };
        let mut found = Vec::new();
        let fresh = changed.iter().filter(|p| {
            repo::is_test_path(p) && repo::is_code(p) && git.original(self.before, p).is_none()
        });
        for p in fresh.take(3) {
            let Ok(text) = std::fs::read_to_string(git.top.join(p)) else { continue };
            let swap = vec![(git.top.join(p), Some(repo::unrunnable(p, &text).into_bytes()))];
            if swap_and_run(self.job, swap, cmd, TEST_TIMEOUT, self.rescuer).await.is_some_and(|run| run.ok()) {
                found.push(p.clone());
            }
        }
        found
    }

    /// A change to existing tests stands only on an explicit pass from a
    /// review of the final change. Anything else, a skipped or garbled
    /// review included, is no agreement.
    #[allow(clippy::too_many_arguments)]
    async fn declared_agreed(
        &self,
        changed: &[String],
        test_files: &[String],
        repro: &Repro,
        without: Option<&Run>,
        edited: &[EditedTest],
        declared: &str,
    ) -> Result<Result<(), String>, SlagError> {
        // Agreement on the test change only: a variant this review names has
        // no revision left to close it, and would sink a verified fix.
        let prompt =
            self.review_prompt(changed, test_files, repro, without, edited, Some(declared), false);
        match review(self.config, self.hooks, self.smith.budget.as_ref(), &prompt).await {
            Ok(Review::Pass(raw)) => Ok(agree_test_edits(&raw, edited, &self.issue.render())),
            Ok(Review::Gap(gap)) => Ok(Err(gap)),
            Ok(Review::Unclear) => Ok(Err("the review gave no clear verdict".into())),
            Err(e) if e.is_deterministic_provider() => Err(e),
            Err(e) => Ok(Err(format!("the review could not run: {e}"))),
        }
    }

    /// `class` adds the bug-class check: only for the review whose gap gets
    /// a revision.
    #[allow(clippy::too_many_arguments)]
    fn review_prompt(
        &self,
        changed: &[String],
        test_files: &[String],
        repro: &Repro,
        without: Option<&Run>,
        edited: &[EditedTest],
        declared: Option<&str>,
        class: bool,
    ) -> String {
        let without_fix = match (repro, without) {
            (Repro::Regression { .. } | Repro::Hangs { .. }, Some(run)) => format!(
                "`{}` gave {}. The end of its output:\n{}",
                run.cmd,
                run.status(),
                repo::tail(&run.output, OUTPUT_TAIL_LINES, OUTPUT_TAIL_BYTES)
            ),
            (Repro::Baseline { status }, _) => {
                format!("not rerun; the test command gave {status} before any edit")
            }
            (Repro::NotShown(why), _) => format!("not shown: {why}"),
            _ => "not run".to_string(),
        };
        let listed = if test_files.is_empty() {
            "(none)".to_string()
        } else {
            test_files.iter().take(60).cloned().collect::<Vec<_>>().join("\n")
        };
        let existing = match declared {
            Some(said) if !edited.is_empty() => format!(
                "The fix rewrites lines of existing tests, and the original tests fail \
                 against it. Its own account: {said}\nExact test changes (- removed, + added):\n{}",
                edited.iter().map(|e| format!("{}:\n{}", e.path, e.diff)).collect::<Vec<_>>().join("\n")
            ),
            _ => "untouched: every line of every pre-existing test is as it was".to_string(),
        };
        crate::flux::fix_review_prompt(
            &self.issue.render(),
            &clip(&self.diff(changed), REVIEW_DIFF_BYTES),
            &listed,
            &without_fix,
            &existing,
            repo::platform(),
            class,
        )
    }
}

/// The one gap a passing fix gets sent back with.
struct Gap {
    text: String,
    kind: GapKind,
}

/// Where a gap came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GapKind {
    /// slag measured it: no test was added.
    NoTest,
    /// slag measured it: the new tests pass with the fix reverted.
    PassesWithoutFix,
    /// slag measured it: a new test file the runner never executes.
    Orphan,
    /// A model's opinion. A revision that follows it must keep passing the
    /// first fix's own tests.
    Review,
}

impl Gap {
    fn new(kind: GapKind, text: &str) -> Self {
        Self { text: text.to_string(), kind }
    }
}

/// Did the session add a test? A new or extended test file, or more test
/// definitions anywhere (Rust keeps unit tests inside the source they
/// test). Rewriting an old test is not adding one.
fn added_test(
    git: &Git,
    before: &BTreeMap<String, repo::Content>,
    changed: &[String],
    edited: &[EditedTest],
) -> bool {
    changed.iter().any(|p| {
        let count = |text: Option<Vec<u8>>| {
            text.map_or(0, |b| repo::test_marks(&String::from_utf8_lossy(&b)).0)
        };
        let more = count(std::fs::read(git.top.join(p)).ok()) > count(git.original(before, p));
        more || (repo::is_test_path(p) && !edited.iter().any(|e| &e.path == p))
    })
}

/// One tool-free call to the judge model. `Some(gap)` on a clear fail with
/// a gap to act on; `None` on a pass, and on a reply too garbled to act on.
/// What one review said.
enum Review {
    Pass(String),
    Gap(String),
    /// A reply with no readable verdict, or a fail with no gap to act on.
    Unclear,
}

async fn review(
    config: &EngineConfig,
    hooks: &EngineHooks,
    budget: Option<&crate::engine::agent::SharedBudget>,
    prompt: &str,
) -> Result<Review, SlagError> {
    use crate::engine::Provider;
    let provider = crate::engine::agent::SpendTracked::new(
        crate::engine::provider::OpenRouter::with_base_url(
            config.api_key.clone(),
            config.base_url.clone(),
        ),
        crate::engine::agent::SpendAccum::default(),
    )
    .with_shared_budget(budget.cloned());
    if let Some(tx) = &hooks.events {
        provider.set_event_sink(tx.clone());
    }
    if let Some(cancel) = &hooks.cancel {
        provider.set_cancel_flag(cancel.clone());
    }
    let call = provider.chat(crate::engine::ChatRequest {
            model: config.model_judge.clone(),
            messages: vec![crate::engine::ChatMessage::user(prompt.to_string())],
            tools: vec![],
            effort: Some(crate::engine::Effort::Low),
            max_tokens: None,
            role: crate::engine::Role::Judge,
            retry: crate::engine::RetryPolicy::side(),
        });
    // Advisory: a slow reviewer must not cost more than the fix it reviews.
    let resp = tokio::time::timeout(REVIEW_TIMEOUT, call).await.map_err(|_| {
        SlagError::Provider(format!("the review took over {}s", REVIEW_TIMEOUT.as_secs()))
    })??;
    Ok(read_review(&resp.content))
}

fn read_review(raw: &str) -> Review {
    if let Some(gap) = review_gap(raw) {
        return Review::Gap(gap);
    }
    // A tool-free review has no inspection evidence to give; the warden's
    // rule that a pass needs evidence does not apply to it.
    let v = super::warden::parse_verdict(raw);
    if raw.contains("VERDICT") && (v.fulfilled || v.gap.contains("without inspection evidence")) {
        return Review::Pass(raw.to_string());
    }
    Review::Unclear
}

#[derive(serde::Deserialize)]
struct TestEditReview {
    path: String,
    removed_behavior_wrong: String,
    added_behavior_requested: String,
    preserves_checks: String,
    issue_quote: String,
    removed_quote: String,
    added_quote: String,
}

/// Lines of a test diff that look like a check (assert, expect, should,
/// require, a comparison, a Go `t.Error`), per sign.
fn check_lines(diff: &str, sign: char) -> usize {
    // Whole words or operators only, and never in a comment: deleting an
    // outdated `# TODO: … should go …` note is not dropping a check.
    static MARKS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let marks = MARKS.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(assert\w*|expect|should|check|require|verify)\b|==|!=|\bt\.(error|fatal)").unwrap()
    });
    let header = if sign == '-' { "---" } else { "+++" };
    diff.lines()
        .filter(|l| l.starts_with(sign) && !l.starts_with(header))
        .map(|l| l[1..].trim_start())
        .filter(|l| !["#", "//", "/*", "*", "--"].iter().any(|c| l.starts_with(c)))
        .filter(|l| marks.is_match(l))
        .count()
}

fn agree_test_edits(raw: &str, edited: &[EditedTest], issue: &str) -> Result<(), String> {
    if let Some(e) = edited.iter().find(|e| !e.diff.lines().any(|l| {
        l.strip_prefix('+').is_some_and(|added| !added.trim().is_empty())
    })) {
        return Err(format!("{} deletes checks without replacement lines", e.path));
    }
    let answers: Vec<&str> = raw.lines().filter_map(|l| l.trim().strip_prefix("TEST_EDITS:")).collect();
    let [answer] = answers.as_slice() else {
        return Err("the review gave no single TEST_EDITS answer".into());
    };
    let rows: Vec<TestEditReview> = serde_json::from_str(answer)
        .map_err(|_| "the review gave an invalid TEST_EDITS answer".to_string())?;
    if rows.len() != edited.len() {
        return Err("the review must answer for each edited test".into());
    }
    for e in edited {
        let (lost, kept) = (check_lines(&e.diff, '-'), check_lines(&e.diff, '+'));
        if kept < lost {
            return Err(format!("{} drops from {lost} check lines to {kept}", e.path));
        }
        let rows: Vec<&TestEditReview> = rows.iter().filter(|r| r.path == e.path).collect();
        let [row] = rows.as_slice() else {
            return Err(format!("the review must answer once for {}", e.path));
        };
        let quoted = |prefix, quote: &str| {
            !quote.trim().is_empty() && e.diff.lines()
                .filter_map(|l| l.strip_prefix(prefix))
                .any(|l| l.trim() == quote.trim())
        };
        if row.removed_behavior_wrong != "yes"
            || row.added_behavior_requested != "yes"
            || row.preserves_checks != "yes"
            || row.issue_quote.trim().is_empty()
            || !issue.contains(&row.issue_quote)
            || !quoted('-', &row.removed_quote)
            || !quoted('+', &row.added_quote)
        {
            return Err(format!("the review did not approve {} with yes answers and matching quotes", e.path));
        }
    }
    Ok(())
}

/// Read the reviewer's reply: a gap only on an explicit fail that names
/// one. Anything else lets the verified fix stand.
fn review_gap(raw: &str) -> Option<String> {
    if !raw.contains("VERDICT") {
        return None;
    }
    let v = super::warden::parse_verdict(raw);
    // The review runs without tools, so a pass carries no inspection
    // evidence; the warden's evidence rule must not turn it into a gap.
    let named = !v.gap.is_empty()
        && !v.gap.contains("without naming a gap")
        && !v.gap.contains("without inspection evidence");
    (!v.fulfilled && named).then_some(v.gap)
}

/// Remove files a session command created by accident: a name that starts
/// with `-` or holds no letter is an argument that landed as a path
/// (`touch -t 202601011200 x` on the wrong platform leaves `202601011200`
/// behind), and a log, temp, or backup file outside the tests is output a
/// trial run left lying around (`./prune.sh > result.log`). Only new files
/// qualify; nothing the repository had is touched.
fn sweep_debris(
    git: &Git,
    before: &BTreeMap<String, repo::Content>,
    changed: &mut Vec<String>,
    notes: &mut Vec<String>,
    hooks: &EngineHooks,
) {
    let debris: Vec<String> = changed
        .iter()
        .filter(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
            let scratch = matches!(ext, "log" | "tmp" | "out" | "bak" | "orig" | "rej" | "swp")
                && !repo::is_test_path(p);
            (name.starts_with('-') || !name.chars().any(|c| c.is_ascii_alphabetic()) || scratch)
                && git.original(before, p).is_none()
        })
        .cloned()
        .collect();
    if debris.is_empty() {
        return;
    }
    for p in &debris {
        let _ = std::fs::remove_file(git.top.join(p));
    }
    changed.retain(|p| !debris.contains(p));
    let line = format!("removed stray files a session command created: {}", debris.join(", "));
    note(hooks, &line, false);
    notes.push(line);
}

/// Each changed file as it is now, so a failed revision can be undone.
fn keep(git: &Git, changed: &[String]) -> Vec<Saved> {
    changed
        .iter()
        .map(|p| {
            let path = git.top.join(p);
            let mode = std::fs::metadata(&path).ok().map(|m| m.permissions());
            let now = std::fs::read(&path).ok();
            (path, now, mode)
        })
        .collect()
}

/// True when the files changed now are exactly the ones `kept` recorded,
/// with the same content.
fn unchanged(git: &Git, kept: &[Saved], now: &[String]) -> bool {
    now.len() == kept.len()
        && now.iter().all(|p| {
            let path = git.top.join(p);
            kept.iter()
                .any(|(k, content, _)| *k == path && *content == std::fs::read(&path).ok())
        })
}

/// Put back the files `kept` recorded, and return anything else the
/// revision touched to its state before the fix.
fn roll_back(
    git: &Git,
    before: &BTreeMap<String, repo::Content>,
    kept: &[Saved],
    now: &[String],
) {
    restore(kept);
    let extra: Vec<Saved> = now
        .iter()
        .filter(|p| !kept.iter().any(|(path, _, _)| *path == git.top.join(p)))
        .map(|p| (git.top.join(p), git.original(before, p), None))
        .collect();
    restore(&extra);
}

/// Name the test files where the session deleted or skipped existing
/// tests: fewer definitions than before, or more skip markers.
fn lost_tests(
    git: &Git,
    before: &BTreeMap<String, repo::Content>,
    changed: &[String],
) -> Option<String> {
    // Every changed file, not only test files: Rust keeps unit tests
    // inside the source they test.
    let hit: Vec<String> = changed
        .iter()
        .filter_map(|p| {
            let old = String::from_utf8(git.original(before, p)?).ok()?;
            let new = std::fs::read_to_string(git.top.join(p)).unwrap_or_default();
            let ((old_defs, old_skips), (new_defs, new_skips)) =
                (repo::test_marks(&old), repo::test_marks(&new));
            (new_defs < old_defs || new_skips > old_skips).then(|| {
                format!("{p} ({old_defs} tests, {old_skips} skipped -> {new_defs}, {new_skips})")
            })
        })
        .collect();
    (!hit.is_empty()).then(|| format!("removed or skipped existing tests in {}", hit.join(", ")))
}

/// Run the verification commands: the repository's test command, then the
/// session's own `cmd` when it differs. Sequential on purpose: two test
/// runs at once fight over build locks and ports.
async fn verify(
    job: &Job,
    hooks: &EngineHooks,
    test: Option<&str>,
    agent: Option<&str>,
) -> Vec<Run> {
    let mut cmds: Vec<&str> = test.into_iter().collect();
    if let Some(a) = agent {
        let same = test.is_some_and(|t| squash(t) == squash(a));
        if !same && !a.trim().is_empty() {
            cmds.push(a);
        }
    }
    let mut runs = Vec::new();
    for cmd in cmds {
        let run = repo::run(cmd, &job.cwd, TEST_TIMEOUT).await;
        note(hooks, &format!("verify: `{cmd}` {} in {:.1}s", run.status(), run.secs), run.ok());
        let ok = run.ok();
        runs.push(run);
        if !ok {
            break;
        }
    }
    runs
}

fn squash(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Show the bug existed. The strongest proof is the session's own
/// regression test failing with the source change taken back out; failing
/// that, a test command that failed before the edit and passes after. The
/// check runs under `timeout`, sized from the passing and baseline runs: a
/// hang shows long before TEST_TIMEOUT, and for a hang bug the timeout
/// itself is the proof.
#[allow(clippy::too_many_arguments)]
async fn reproduce(
    job: &Job,
    before: &BTreeMap<String, repo::Content>,
    changed: &[String],
    edited: &[String],
    cmd: &str,
    timeout: Duration,
    baseline: Option<&Run>,
    rescuer: &Rescuer<'_>,
) -> (Repro, Option<Run>) {
    let from_baseline = || match baseline {
        Some(b) if !b.ok() && !b.unusable() => Some(Repro::Baseline { status: b.status() }),
        _ => None,
    };
    let Some(git) = &job.git else {
        return (from_baseline().unwrap_or(Repro::NotShown("not a git repository".into())), None);
    };
    let (tests, sources): (Vec<&String>, Vec<&String>) =
        changed.iter().partition(|p| repo::is_test_path(p));
    let why = if tests.is_empty() {
        Some("the fix added no test")
    } else if sources.is_empty() {
        Some("only test files changed")
    } else {
        None
    };
    if let Some(why) = why {
        return (from_baseline().unwrap_or_else(|| Repro::NotShown(why.into())), None);
    }

    // A symlink would come back as a copy of its target. Not worth the
    // risk for a proof: say so instead.
    if sources.iter().any(|p| {
        std::fs::symlink_metadata(git.top.join(p)).is_ok_and(|m| m.file_type().is_symlink())
    }) {
        return (from_baseline().unwrap_or(Repro::NotShown("the fix changed a symlink".into())), None);
    }

    // The source goes back to its pre-session content, and so does any
    // pre-existing test the session edited: the bug must be caught by the
    // new tests, never by old ones rewritten to fit the fix.
    let olds: Vec<&String> = edited.iter().filter(|p| !sources.contains(p)).collect();
    let swaps: Vec<(PathBuf, repo::Content)> = sources
        .iter()
        .chain(&olds)
        .map(|p| (git.top.join(p), git.original(before, p)))
        .collect();
    let Some(run) = swap_and_run(job, swaps, cmd, timeout, rescuer).await else {
        return (Repro::NotShown("interrupted".into()), None);
    };

    let repro = if run.timed_out {
        // The same check passed with the fix: never finishing without it
        // is the hang the issue reports, not a check that says nothing.
        Repro::Hangs { status: run.status() }
    } else if run.unusable() {
        from_baseline()
            .unwrap_or_else(|| Repro::NotShown(format!("the check without the fix {}", run.status())))
    } else if run.ok() {
        match inside(job, before, &sources, &olds, cmd, timeout, rescuer, &run).await {
            None => return (Repro::NotShown("interrupted".into()), None),
            Some(Ok((repro, run))) => return (repro, Some(run)),
            Some(Err(why)) => from_baseline().unwrap_or(Repro::NotShown(why)),
        }
    } else {
        Repro::Regression { status: run.status() }
    };
    (repro, Some(run))
}

/// New tests the session put inside a pre-existing test file it also
/// edited: a unittest class, a table of cases. Restored whole, the file
/// loses them, so when that check passes, run it again with every line the
/// session deleted or rewrote back as it was and only the lines it
/// inserted kept. A failure there proves the bug only when it is clean:
/// nothing fails to load that loaded in `restored`, and the session's
/// tests without the kept lines still pass with the fix, so those lines
/// are new tests, not an old expectation that grew. `Err` says why there
/// is no proof; `None` means the rescue ran.
#[allow(clippy::too_many_arguments)]
async fn inside(
    job: &Job,
    before: &BTreeMap<String, repo::Content>,
    sources: &[&String],
    olds: &[&String],
    cmd: &str,
    timeout: Duration,
    rescuer: &Rescuer<'_>,
    restored: &Run,
) -> Option<Result<(Repro, Run), String>> {
    let mut why = "the new test also passes without the fix".to_string();
    let Some(git) = &job.git else { return Some(Err(why)) };
    let texts: Vec<(&String, String, String)> = olds
        .iter()
        .filter_map(|p| {
            let old = String::from_utf8(git.original(before, p)?).ok()?;
            Some((*p, old, std::fs::read_to_string(git.top.join(p)).ok()?))
        })
        .collect();
    let mut tried = Vec::new();
    // A rewritten block that grew reads two ways: an old test edited and a
    // new one added after it, or an old expectation that grew. Try both in
    // that order; the checks below reject a wrong reading.
    for split in [true, false] {
        let parts: Vec<(&String, String, String)> = texts
            .iter()
            .filter_map(|(p, old, new)| repo::insertions(old, new, split).map(|(kept, rest)| (*p, kept, rest)))
            .collect();
        if parts.is_empty() || tried.contains(&parts) {
            continue;
        }
        let names = parts.iter().map(|(p, ..)| p.as_str()).collect::<Vec<_>>().join(", ");
        let swaps = sources
            .iter()
            .map(|p| (git.top.join(p), git.original(before, p)))
            .chain(olds.iter().map(|p| {
                let kept = parts.iter().find(|(q, ..)| q == p).map(|(_, kept, _)| kept.clone().into_bytes());
                (git.top.join(p), kept.or_else(|| git.original(before, p)))
            }))
            .collect();
        let run = swap_and_run(job, swaps, cmd, timeout, rescuer).await?;
        if run.ok() {
            // Fewer kept lines cannot fail where more passed.
            return Some(Err(why));
        }
        let broken = repo::load_errors(&run.output)
            .into_iter()
            .find(|e| !restored.output.contains(e))
            .map(str::to_string)
            .or_else(|| (run.unusable() && !run.timed_out).then(|| run.status()));
        if let Some(e) = broken {
            why = format!("the tests added inside {names} do not run with the old lines back ({e})");
            tried.push(parts);
            continue;
        }
        let control = parts.iter().map(|(p, _, rest)| (git.top.join(p), Some(rest.clone().into_bytes()))).collect();
        let check = swap_and_run(job, control, cmd, timeout, rescuer).await?;
        if !check.ok() {
            why = format!(
                "the lines added inside {names} belong to old tests: without them, `{cmd}` gives {} with the fix",
                check.status()
            );
            tried.push(parts);
            continue;
        }
        let repro = if run.timed_out {
            Repro::Hangs { status: run.status() }
        } else {
            Repro::Regression { status: run.status() }
        };
        return Some(Ok((repro, run)));
    }
    Some(Err(why))
}

/// Put `swaps` on disk in place of the current files, run `cmd`, and put
/// the current files back. `None` when the rescue ran in between: the
/// current files are on disk then, and the run proves nothing. A Ctrl-C at
/// any point must not cost the user the fix, so the rescue is registered
/// first, and every swap and the rescue take one lock: see `Swap`.
async fn swap_and_run(
    job: &Job,
    swaps: Vec<(PathBuf, repo::Content)>,
    cmd: &str,
    timeout: Duration,
    rescuer: &Rescuer<'_>,
) -> Option<Run> {
    let fixed: Vec<Saved> = swaps
        .iter()
        .map(|(path, _)| {
            let mode = std::fs::metadata(path).ok().map(|m| m.permissions());
            (path.clone(), std::fs::read(path).ok(), mode)
        })
        .collect();
    let swap = Arc::new(Mutex::new(Swap { phase: Phase::Armed, fixed }));
    let rescue = swap.clone();
    let stop = rescuer.stop;
    (rescuer.register)(Box::new(move || Swap::rescue(&rescue, stop)));
    let originals: Vec<Saved> = swaps.into_iter().map(|(path, old)| (path, old, None)).collect();
    {
        let mut s = swap.lock().unwrap_or_else(|e| e.into_inner());
        if s.phase != Phase::Armed {
            return None;
        }
        restore(&originals);
        s.phase = Phase::Swapped;
    }
    let run = repo::run(cmd, &job.cwd, timeout).await;
    let mut s = swap.lock().unwrap_or_else(|e| e.into_inner());
    if s.phase != Phase::Swapped {
        // The rescue ran during the check and already put the fix back.
        return None;
    }
    restore(&s.fixed);
    s.phase = Phase::Done;
    Some(run)
}

/// A pre-existing test the session deleted or rewrote lines of.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditedTest {
    path: String,
    /// The lines it lost, trimmed.
    removed: Vec<String>,
    /// The complete test diff, including indentation and added lines.
    diff: String,
    /// What the original tests look like against the session's source:
    /// the whole original file, or, for a source file with an inline test
    /// module, the session's code above the module and the original
    /// module below it.
    original_tests: repo::Content,
}

/// Pre-existing tests the session deleted or rewrote: test files, and the
/// inline test module of a source file (Rust keeps unit tests there).
/// Pure additions are fine and do not appear here.
fn edited_old_tests(
    git: &Git,
    before: &BTreeMap<String, repo::Content>,
    changed: &[String],
) -> Vec<EditedTest> {
    changed
        .iter()
        .filter_map(|p| {
            let old = String::from_utf8(git.original(before, p)?).ok()?;
            let new = std::fs::read(git.top.join(p)).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
            if repo::is_test_path(p) {
                let removed = repo::removed_lines(&old, new.as_deref().unwrap_or(""));
                return (!removed.is_empty()).then(|| EditedTest {
                    path: p.clone(),
                    removed,
                    diff: similar::TextDiff::from_lines(old.as_str(), new.as_deref().unwrap_or(""))
                        .unified_diff().to_string(),
                    original_tests: Some(old.clone().into_bytes()),
                });
            }
            let (_, old_tests) = repo::split_inline_tests(&old)?;
            let new = new?;
            let (new_code, new_tests) = repo::split_inline_tests(&new).unwrap_or((&new, ""));
            let removed = repo::removed_lines(old_tests, new_tests);
            (!removed.is_empty()).then(|| EditedTest {
                path: p.clone(),
                removed,
                diff: similar::TextDiff::from_lines(old_tests, new_tests).unified_diff().to_string(),
                original_tests: Some(format!("{new_code}{old_tests}").into_bytes()),
            })
        })
        .collect()
}

/// Does any line an edited test lost name something the issue names: a
/// code name, a quoted value, or a number? Words every test uses (assert,
/// expect, equal, test) do not count.
fn ties_to_issue(edited: &[EditedTest], issue: &str) -> bool {
    const COMMON: &[&str] = &[
        "assert", "assertEqual", "assert_eq", "expect", "equal", "toBe", "toEqual", "test",
        "self", "true", "false", "None", "null", "return", "should", "const", "let",
    ];
    let token = regex::Regex::new(r#"[A-Za-z_][A-Za-z0-9_]{2,}|\d+(?:\.\d+)?|"[^"]+"|'[^']+'"#).unwrap();
    let issue_lower = issue.to_lowercase();
    edited.iter().flat_map(|e| &e.removed).any(|line| {
        token.find_iter(line).any(|m| {
            let t = m.as_str().trim_matches(|c| c == '"' || c == '\'');
            !t.is_empty()
                && !COMMON.contains(&t)
                && t.len() >= 2
                && issue_lower.contains(&t.to_lowercase())
        })
    })
}

/// The lines each edited test lost, for a prompt or a report.
fn removed_text(edited: &[EditedTest]) -> String {
    let mut s = String::new();
    for e in edited {
        s.push_str(&format!("{}:\n", e.path));
        for line in e.removed.iter().take(30) {
            s.push_str(&format!("- {line}\n"));
        }
        if e.removed.len() > 30 {
            s.push_str(&format!("- ... and {} more\n", e.removed.len() - 30));
        }
    }
    s
}

/// The session's own account of a deliberate change to existing tests: the
/// summary lines that say `changed existing tests:`, when together they
/// name every edited file. `None` when the change went unsaid.
fn declaration(summary: &str, edited: &[EditedTest]) -> Option<String> {
    let lines: Vec<&str> = summary
        .lines()
        .map(str::trim)
        .filter(|l| l.to_ascii_lowercase().contains("changed existing tests"))
        .collect();
    let said = lines.join(" ");
    let named = edited.iter().all(|e| {
        let name = e.path.rsplit('/').next().unwrap_or(&e.path);
        said.contains(e.path.as_str()) || said.contains(name)
    });
    (!lines.is_empty() && named).then(|| lines.join("; "))
}

/// Where `reproduce` hands its rescue, and how the rescue stops a check
/// still running. A real fix registers with the shutdown hooks and kills
/// slag's commands; unit tests keep the rescue in a local slot, so no test
/// can fire another test's rescue.
struct Rescuer<'a> {
    register: &'a dyn Fn(Box<dyn FnOnce() + Send>),
    stop: fn(),
}

fn register_shutdown(hook: Box<dyn FnOnce() + Send>) {
    crate::shutdown::register(hook);
}

/// Where the reproduction swap stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The fix is on disk and nothing is swapped yet.
    Armed,
    /// The pre-session sources are on disk while the check runs.
    Swapped,
    /// The fix is back and the rescue has nothing to do.
    Done,
    /// The rescue ran. The fix is on disk, and no swap may start again.
    Rescued,
}

/// The swap state and the fix's bytes, behind one lock shared by
/// `reproduce` and its rescue. Deciding to swap and swapping happen under
/// that lock, so the rescue cannot run between them, and the originals
/// are never written once the rescue has put the fix back.
struct Swap {
    phase: Phase,
    fixed: Vec<Saved>,
}

impl Swap {
    fn rescue(swap: &Mutex<Swap>, stop: fn()) {
        let mut s = swap.lock().unwrap_or_else(|e| e.into_inner());
        if s.phase == Phase::Swapped {
            // The check is still going: stop it before it can write over
            // the restore.
            stop();
            restore(&s.fixed);
        }
        if s.phase != Phase::Done {
            s.phase = Phase::Rescued;
        }
    }
}

/// One file as the reproduction check found it: path, content, and mode
/// (`None` keeps whatever mode the file has).
type Saved = (PathBuf, repo::Content, Option<std::fs::Permissions>);

/// Write each path's content back, removing paths whose content is `None`.
/// A file recreated from nothing gets its mode back too, or a new script
/// would lose its executable bit on the way through. Every swap drops the
/// file's Python bytecode: a same-size swap inside one second otherwise
/// leaves a `.pyc` that runs the other version of the code. Every write
/// also gets a `swap_mtime`, so no cache keyed on whole-second mtimes (one
/// under `PYTHONPYCACHEPREFIX` included) can mistake one version for the
/// other.
fn restore(files: &[Saved]) {
    for (path, content, mode) in files {
        repo::purge_bytecode(path);
        match content {
            Some(bytes) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, bytes);
                if let Ok(file) = std::fs::File::options().write(true).open(path) {
                    let _ = file.set_modified(swap_mtime(path));
                }
                if let Some(mode) = mode {
                    let _ = std::fs::set_permissions(path, mode.clone());
                }
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// The mtime a swapped file gets: the clock, unless this file's previous
/// swap already used this whole second, in which case the next whole
/// second. Two versions of a file then never share an mtime second, the
/// clock is never truncated (a build tool still sees the fix as newer than
/// anything compiled from the old source), and a file runs at most a
/// second ahead of the clock.
fn swap_mtime(path: &std::path::Path) -> std::time::SystemTime {
    use std::time::{SystemTime, UNIX_EPOCH};
    static LAST: Mutex<BTreeMap<PathBuf, SystemTime>> = Mutex::new(BTreeMap::new());
    let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = SystemTime::now();
    let stamp = match last.get(path) {
        Some(prev) if secs(now) <= secs(*prev) => {
            UNIX_EPOCH + Duration::from_secs(secs(*prev) + 1)
        }
        _ => now,
    };
    last.insert(path.to_path_buf(), stamp);
    stamp
}

/// Commit the verified fix on a new branch, and open a draft PR when asked.
/// A failure here is reported, never fatal: the fix itself is on disk.
fn land(job: &Job, out: &mut Outcome, changed: &[String], before: &BTreeMap<String, repo::Content>) {
    out.files = changed.to_vec();
    // The user's own uncommitted files stay where they were, out of the
    // commit, and the report says so.
    if !before.is_empty() {
        let moved = job.git.as_ref().map(|g| g.yours_changed(before)).unwrap_or_default();
        let mut line = format!("left untouched: {} files you had before", before.len());
        if !moved.is_empty() {
            line.push_str(&format!(
                " ({} changed during the run and stay uncommitted)",
                moved.join(", ")
            ));
        }
        out.notes.push(line);
    }
    let Some(git) = job.git.as_ref().filter(|g| !g.is_shadow()) else {
        out.notes.push("not a git repository: the fix is in the working tree".into());
        return;
    };
    if job.args.no_commit {
        out.notes.push("--no-commit: the fix is in the working tree".into());
        return;
    }
    let branch = git.free_branch(&out.issue.branch_name());
    let message = commit_message(out);
    match git.commit_on_branch(&branch, changed, &message) {
        Ok(sha) => {
            out.stat = git
                .git(&["show", "--stat", "--format=", "HEAD"])
                .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
                .unwrap_or_else(|| out.stat.clone());
            out.branch = Some(branch.clone());
            out.commit = Some(sha);
        }
        Err(e) => {
            out.notes.push(format!("commit failed, the fix is in the working tree: {e}"));
            return;
        }
    }
    if job.args.pr {
        match open_pr(git, &branch, out) {
            Ok(url) => out.pr = Some(url),
            Err(e) => out.notes.push(format!("draft PR not opened: {e}")),
        }
    }
}

fn commit_message(out: &Outcome) -> String {
    let mut msg = format!("fix: {}\n", tui::truncate(&out.issue.title, 68));
    if !out.summary.trim().is_empty() {
        msg.push_str(&format!("\n{}\n", out.summary.trim()));
    }
    for check in &out.checks {
        msg.push_str(&format!("\nVerified: `{}` {}", check.cmd, check.status()));
    }
    msg.push('\n');
    if let Some(n) = out.issue.closing_ref() {
        msg.push_str(&format!("\nFixes {n}\n"));
    }
    msg
}

/// Push the branch and open a draft PR whose body carries the
/// reproduction, the fix, and the verification output.
fn open_pr(git: &Git, branch: &str, out: &Outcome) -> Result<String, String> {
    let remote = git
        .git(&["remote"])
        .and_then(|r| {
            let names: Vec<String> = r.lines().map(str::to_string).collect();
            names.iter().find(|n| *n == "origin").or(names.first()).cloned()
        })
        .ok_or("the repository has no remote")?;
    git.git_checked(&["push", "--quiet", "-u", &remote, branch])?;
    let body_path = std::env::temp_dir().join(format!("slag-fix-pr-{}.md", std::process::id()));
    std::fs::write(&body_path, pr_body(out)).map_err(|e| e.to_string())?;
    let created = std::process::Command::new("gh")
        .envs(crate::engine::sandbox::GIT_GUARD_ENV)
        .current_dir(&git.top)
        .args(["pr", "create", "--draft", "--head", branch, "--title"])
        .arg(format!("fix: {}", out.issue.title))
        .arg("--body-file")
        .arg(&body_path)
        .output()
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&body_path);
    let created = created?;
    if !created.status.success() {
        return Err(String::from_utf8_lossy(&created.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&created.stdout).trim().to_string())
}

fn pr_body(out: &Outcome) -> String {
    let mut body = String::from("## Issue\n\n");
    match out.issue.closing_ref() {
        Some(r) => body.push_str(&format!("Fixes {r}\n")),
        None => body.push_str(&format!("> {}\n", out.issue.title)),
    }
    body.push_str("\n## Reproduction\n\n");
    body.push_str(&repro_line(&out.repro, out.checks.first().map(|c| c.cmd.as_str())));
    body.push_str("\n\n## Fix\n\n");
    body.push_str(out.summary.trim());
    if !out.files.is_empty() {
        body.push_str("\n\nFiles in this change:\n");
        for f in &out.files {
            body.push_str(&format!("- `{f}`\n"));
        }
    }
    body.push_str("\n\n## Verification\n");
    for check in &out.checks {
        body.push_str(&format!(
            "\n`{}`: {} in {:.1}s\n\n```\n{}\n```\n",
            check.cmd,
            check.status(),
            check.secs,
            repo::tail(&check.output, 20, 2_000)
        ));
    }
    body.push_str("\nOpened by `slag fix`. Every verdict above comes from a command slag ran.\n");
    body
}

fn repro_line(repro: &Repro, cmd: Option<&str>) -> String {
    let cmd = cmd.unwrap_or("the tests");
    match repro {
        Repro::Regression { status } => {
            format!("With the source change reverted, `{cmd}` gives {status}: the new test catches the bug.")
        }
        Repro::Hangs { status } => {
            format!("With the source change reverted, `{cmd}` never finishes ({status}): the code hangs without the fix.")
        }
        Repro::Baseline { status } => format!("Before the fix, `{cmd}` gave {status}."),
        Repro::NotShown(why) => format!("Not shown: {why}."),
    }
}

/// The warden judges the fix on disk against the issue. It never sees the
/// fixer's summary.
async fn judge(
    config: &EngineConfig,
    hooks: &EngineHooks,
    issue: &Issue,
    verify: &str,
) -> Result<super::warden::Verdict, SlagError> {
    let smith = crate::smith::make_warden(config, hooks);
    let raw = smith
        .invoke(&crate::flux::fix_warden_prompt(&issue.render(), verify))
        .await?;
    Ok(super::warden::parse_verdict(&raw))
}

/// What slag already knows about the repository, so the session does not
/// spend turns finding it.
fn context_block(
    job: &Job,
    issue: &Issue,
    files: &[String],
    test_files: &[String],
    test: Option<&TestCmd>,
    baseline: Option<&Run>,
) -> String {
    let mut s = String::from("=== REPOSITORY ===\n");
    // Naming the likely files lets the first turn read them all at once
    // instead of spending a turn finding them.
    let likely = repo::rank_files(&job.cwd, files, &repo::issue_terms(&issue.render()), 6);
    if !likely.is_empty() {
        s.push_str(&format!(
            "Files that mention the names in the issue: {}\n\n",
            likely.join(", ")
        ));
    }
    let mut listed = String::new();
    let mut shown = 0;
    for f in files.iter().take(FILE_LIST_CAP) {
        if listed.len() + f.len() > FILE_LIST_BYTES {
            break;
        }
        listed.push_str(f);
        listed.push('\n');
        shown += 1;
    }
    if shown < files.len() {
        s.push_str(&format!("{} files; the first {shown}:\n", files.len()));
    } else {
        s.push_str(&format!("{} files:\n", files.len()));
    }
    s.push_str(&listed);

    for name in [crate::config::ALLOY_FILE, "CLAUDE.md"] {
        if let Ok(text) = std::fs::read_to_string(job.cwd.join(name)) {
            if text.trim().is_empty() {
                continue;
            }
            s.push_str(&format!(
                "\n=== PROJECT INSTRUCTIONS ===\n{}\nContents of {name}:\n{}\n",
                crate::flux::INSTRUCTION_OVERRIDE_HEADER,
                clip(&text, INSTRUCTIONS_BYTES)
            ));
            break;
        }
    }

    s.push_str("\n=== TESTS ===\n");
    s.push_str(&format!("Platform: {}\n", repo::platform()));
    if test_files.is_empty() {
        s.push_str("Test files: none yet.\n");
    } else {
        let shown: Vec<&str> = test_files.iter().take(40).map(String::as_str).collect();
        s.push_str(&format!("Test files: {}\n", shown.join(", ")));
    }
    // Where a test for each likely file goes, so a test never lands in
    // the file for another module.
    let homes: Vec<String> = likely
        .iter()
        .filter(|f| !repo::is_test_path(f))
        .take(3)
        .filter_map(|f| repo::test_home(f, test_files).map(|home| format!("- {f}: {home}")))
        .collect();
    if !homes.is_empty() {
        s.push_str(&format!("Where a test for each likely file goes:\n{}\n", homes.join("\n")));
    }
    match (test, baseline) {
        (Some(t), Some(b)) if b.unusable() => s.push_str(&format!(
            "`{}` (from {}) could not run here: {}. Find the command this repository \
             really uses and put it in the finish `cmd`.\n{}\n",
            t.cmd,
            t.source,
            b.status(),
            repo::tail(&b.output, 10, 1_000)
        )),
        (Some(t), Some(b)) => {
            s.push_str(&format!(
                "Test command (from {}): {}\nslag ran it before any edit: {} in {:.1}s. The end \
                 of its output:\n{}\n",
                t.source,
                t.cmd,
                b.status(),
                b.secs,
                repo::tail(&b.output, OUTPUT_TAIL_LINES, OUTPUT_TAIL_BYTES)
            ));
            if let Some(wiring) = repo::runner_wiring(&job.cwd, &t.cmd) {
                s.push_str(&format!("How the runner finds tests: {wiring}\n"));
            }
        }
        _ => s.push_str(
            "No test command was detected. Find how this repository runs its tests \
             (README, CI config, manifests) and put that command in the finish `cmd`.\n",
        ),
    }
    s
}

/// The retry's starting point: what failed, and what is already on disk.
fn retry_block(failure: &str, diff: &str) -> String {
    let mut s = format!("=== PREVIOUS ATTEMPT FAILED ===\n{}", failure.trim());
    if !diff.trim().is_empty() {
        s.push_str(&format!(
            "\n\nThe working tree still holds that attempt's changes:\n{}\n\nStart from this \
             state. Find out why it fails and fix that; start over only if the approach is \
             wrong.",
            clip(diff, RETRY_DIFF_BYTES)
        ));
    }
    s
}

/// The revision's brief: the fix passes, one gap is left, and the change
/// that passed is the starting point.
fn revision_block(gap: &str, diff: &str) -> String {
    format!(
        "=== REVIEW: ONE GAP LEFT ===\nYour fix passes the tests, and a reviewer read it. \
         One gap remains:\n{}\n\nClose exactly that gap and change nothing else. The working \
         tree holds your change:\n{}\n\nKeep what is right. Run the full test command again \
         before you finish.",
        gap.trim(),
        clip(diff, RETRY_DIFF_BYTES)
    )
}

fn clip(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_string();
    }
    let mut cut = bytes;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n... (cut at {bytes} bytes)", &text[..cut])
}

/// The finish summary without the `CMD:` line the finish tool appends.
fn strip_cmd_lines(response: &str) -> String {
    response
        .lines()
        .filter(|l| !l.trim_start().starts_with("CMD:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// One progress line: to the dashboard as narration or a warning, and to
/// the terminal in stream mode.
fn note(hooks: &EngineHooks, text: &str, good: bool) {
    if good {
        emit(&hooks.events, EngineEvent::Narrate { text: text.to_string() });
        tui::status_line("✓", tui::COLD, text);
    } else {
        emit(&hooks.events, EngineEvent::Warning { message: text.to_string() });
        tui::status_line("!", tui::WARM, text);
    }
}

/// Record run time and spend as of now. The stats are process-wide, so a
/// second fix under one dashboard reports the running total.
fn stamp(out: &mut Outcome) {
    let run = stats::snapshot();
    out.time = stats::durations_line(&run);
    out.spend = stats::ledger_lines(&run)
        .map(|_| crate::engine::pricing::format_cost(&run.ledger.total()));
}

/// The fix's report card, laid out like the forge's assay: one verdict,
/// the next command, then the evidence (each check with its exit code,
/// the reproduction, the diff stat, where the commit is, what it took).
fn report(out: &Outcome) {
    if tui::is_quiet() {
        return;
    }
    let fixed = out.verdict == Verdict::Fixed;
    tui::header(if fixed { "FIX · FIXED" } else { "FIX · NOT FIXED" });
    for line in report_lines(out, tui::width()) {
        println!("{line}");
    }
}

/// The issue's first line as written. The stored title is capped for
/// branch names and commit subjects; the report has the room to show the
/// whole line, so it takes it from the body when the title was cut.
fn issue_line(issue: &Issue) -> String {
    let stem = issue.title.trim_end_matches(['…', '.']).trim_end();
    issue
        .body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .filter(|first| first.starts_with(stem) && first.len() > issue.title.len())
        .map(str::to_string)
        .unwrap_or_else(|| issue.title.clone())
}

/// The report card's lines under its header. Every glyph is painted on
/// its own and every value clipped as plain text, so no escape sequence
/// is ever cut in half on its way to the screen.
fn report_lines(out: &Outcome, w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let label = |name: &str| tui::dim(&format!("{name:<6}"));
    // Evidence wraps under its label instead of trailing off in `…`.
    let wrapped = |name: &str, value: &str, max: usize| tui::wrap(&format!("  {}  ", label(name)), value, w, 10, max);
    let title = match out.issue.number {
        Some(n) => format!("#{n} {}", issue_line(&out.issue)),
        None => issue_line(&out.issue),
    };
    let attempts = plural(out.attempts, "attempt");

    // The verdict and the attempt count, alone on their line; the issue
    // and the reason follow whole, on lines of their own.
    match &out.verdict {
        Verdict::Fixed => {
            lines.push(format!(
                "  {} {} {}",
                tui::paint(tui::PURE, "✓"),
                tui::paint(tui::PURE, &format!("{}FIXED{}", bold(), reset())),
                tui::dim(&format!("in {attempts}"))
            ));
            lines.extend(wrapped("issue", &title, 3));
        }
        Verdict::Failed(reason) => {
            lines.push(format!(
                "  {} {} {}",
                tui::paint(tui::WARM, "✗"),
                tui::paint(tui::WARM, &format!("{}NOT FIXED{}", bold(), reset())),
                tui::dim(&format!("after {attempts}"))
            ));
            lines.extend(wrapped("why", reason, 4).into_iter().map(|l| tui::paint(tui::WARM, &l)));
            lines.extend(wrapped("issue", &title, 3));
        }
    }

    // The one thing to do next.
    let next = match (&out.verdict, &out.pr, &out.branch) {
        (Verdict::Fixed, Some(pr), _) => (format!("open {pr}"), "the pull request".to_string()),
        (Verdict::Fixed, None, Some(_)) => ("git show --stat HEAD".to_string(), "review the commit".to_string()),
        (Verdict::Fixed, None, None) => ("git diff".to_string(), "review the change".to_string()),
        (Verdict::Failed(_), _, _) => (
            format!("slag fix --tries {} …", out.attempts.saturating_add(2)),
            "the same issue, more attempts".to_string(),
        ),
    };
    let room = w.saturating_sub(12 + next.0.chars().count());
    lines.push(format!("  {}  {}  {}", label("next"), tui::paint(tui::HOT, &next.0), tui::dim(&tui::clip(&next.1, room.max(4)))));
    lines.push(String::new());

    // What ran, and how it exited: a glyph, the command, the exit code.
    let proof_row = |first: bool, ok: bool, text: &str, meta: &str| {
        let (glyph, color) = if ok { ("✓", tui::PURE) } else { ("✗", tui::WARM) };
        let left = format!(
            "  {}  {} {}",
            label(if first { "proof" } else { "" }),
            tui::paint(color, glyph),
            tui::clip(text, w.saturating_sub(16 + meta.chars().count()))
        );
        if meta.is_empty() {
            left
        } else {
            tui::beside(&left, &tui::paint(if ok { tui::COLD } else { tui::WARM }, meta), w)
        }
    };
    for (i, check) in out.checks.iter().enumerate() {
        let meta = format!("{} · {:.1}s", check.status(), check.secs);
        lines.push(proof_row(i == 0, check.ok(), &check.cmd, &meta));
    }
    if out.verdict == Verdict::Fixed {
        let first = out.checks.is_empty();
        match &out.repro {
            // A failing run without the fix is the evidence the fix works:
            // it reads as a failure, in the failure's colour, like any other.
            Repro::Regression { status } => {
                lines.push(proof_row(first, false, &format!("without the fix: {status}, so the new test catches the bug"), ""))
            }
            Repro::Hangs { status } => {
                lines.push(proof_row(first, false, &format!("without the fix: {status}, so the code hangs"), ""))
            }
            Repro::Baseline { status } => lines.push(proof_row(first, false, &format!("before the fix: {status}"), "")),
            Repro::NotShown(why) => lines.push(format!(
                "  {}  {} {}",
                label(if first { "proof" } else { "" }),
                tui::dim("·"),
                tui::clip(&format!("reproduction not shown: {why}"), w.saturating_sub(12))
            )),
        }
    }
    if let Some(bad) = out.checks.iter().find(|c| !c.ok()) {
        for line in repo::tail(&bad.output, 8, 1_500).lines() {
            lines.push(format!("            {}", tui::dim(&tui::clip(line, w.saturating_sub(12)))));
        }
    }

    // What changed, and where it is.
    for (i, line) in out.stat.iter().enumerate() {
        lines.extend(wrapped(if i == 0 { "diff" } else { "" }, line.trim(), 2));
    }
    let mut work = Vec::new();
    if let (Some(branch), Some(sha)) = (&out.branch, &out.commit) {
        work.push(format!("{branch} @ {sha}"));
    }
    if let Some(pr) = &out.pr {
        work.push(pr.clone());
    }
    if !work.is_empty() {
        lines.extend(wrapped("work", &work.join(" · "), 2));
    }
    for n in &out.notes {
        // The live review line already gave the gap in full; once the
        // revision closed it, the card says only that.
        let n = if n.starts_with("revised once after review: ") { "revised once after review; the revision closed the gap" } else { n };
        lines.extend(wrapped("note", n, 3));
    }
    if let Some(ev) = &out.evolve {
        lines.extend(wrapped("evolve", &ev.head, 3));
        for line in ev.stages.iter().chain(&ev.lines) {
            lines.extend(wrapped("", line, 2));
        }
    }

    // What it took.
    let mut took = Vec::new();
    if let Some(line) = &out.time {
        took.push(line.clone());
    }
    if let Some(spend) = out.spend.as_deref().filter(|s| s.trim() != "-") {
        took.push(spend.to_string());
    }
    if !took.is_empty() {
        lines.extend(wrapped("run", &took.join(" · "), 2));
    }
    let phases = out.phases.line();
    if !phases.is_empty() {
        lines.extend(wrapped("", &phases, 2));
    }
    lines.push(String::new());
    lines
}

#[cfg(test)]
mod report_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn job_in(dir: &Path) -> Job {
        Job {
            args: FixArgs {
                issue: vec![],
                issue_file: None,
                test: None,
                tries: 1,
                pr: false,
                no_commit: false,
                temper: false,
                model: None,
                plan_model: None,
                tui: false,
                evolve: 0,
                sequential: false,
                harness: None,
                editor_model: None,
                evolve_budget: None,
                evolve_tokens: None,
                domain: None,
                library: None,
            },
            temper: false,
            cwd: dir.to_path_buf(),
            git: Git::open(dir),
        }
    }

    fn sh(dir: &Path, cmd: &str) {
        let ok = std::process::Command::new("bash")
            .args(["-c", cmd])
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "{cmd}");
    }

    #[tokio::test]
    async fn auth_failure_stops_fix_before_a_plan_model_retry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_guard, _config_dir) = crate::config::isolated_config_dir();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "message": "Wrong API Key" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let provider = crate::config::active_provider().unwrap();
        let config = EngineConfig {
            api_key: "test-key".into(),
            model_base: format!("{}::work-model", provider.name()),
            model_plan: format!("{}::plan-model", provider.name()),
            model_alt: format!("{}::alt-model", provider.name()),
            model_judge: format!("{}::judge-model", provider.name()),
            effort: None,
            base_url: server.uri(),
            duel: crate::config::DuelMode::Off,
            duel_rounds_override: None,
            screenshot_cmd: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut job = job_in(dir.path());
        job.args.issue = vec!["Fix the test".into()];
        job.args.test = Some("true".into());
        job.args.tries = 3;
        let (tx, _rx) = crate::engine::events::channel();
        let hooks = EngineHooks { events: Some(tx), ..EngineHooks::default() };

        let error = match fix(&job, &config, &hooks).await {
            Err(error) => error,
            Ok(_) => panic!("authentication failure must stop the fix"),
        };
        assert!(error.is_deterministic_provider(), "{error}");
        let requests = server.received_requests().await.unwrap();
        let calls: Vec<_> = requests.iter().filter(|r| r.method == "POST").collect();
        assert_eq!(calls.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&calls[0].body).unwrap();
        assert_eq!(body["model"], "work-model");
    }

    #[test]
    fn the_finish_cmd_line_stays_out_of_the_summary() {
        assert_eq!(strip_cmd_lines("Root cause: a.py:3\nFixed it.\nCMD: npm test"), "Root cause: a.py:3\nFixed it.");
    }

    #[test]
    fn the_context_names_the_tests_and_how_they_ended() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "").unwrap();
        let job = job_in(dir.path());
        let test = TestCmd { cmd: "python3 -m unittest".into(), source: "Python tests".into() };
        let base = Run {
            cmd: test.cmd.clone(),
            code: Some(1),
            output: "FAIL: test_x\nAssertionError".into(),
            secs: 0.2,
            timed_out: false,
        };
        let issue = Issue::from_text("crash in `helper_fn`");
        std::fs::write(dir.path().join("b.py"), "def helper_fn(): pass\n").unwrap();
        let files = vec!["a.py".to_string(), "b.py".to_string()];
        let ctx = context_block(&job, &issue, &files, &[], Some(&test), Some(&base));
        assert!(ctx.contains("mention the names in the issue: b.py"), "{ctx}");
        assert!(ctx.contains("a.py"), "{ctx}");
        assert!(ctx.contains("python3 -m unittest") && ctx.contains("exit 1"), "{ctx}");
        assert!(ctx.contains("AssertionError"), "{ctx}");

        let none = context_block(&job, &issue, &files, &[], None, None);
        assert!(none.contains("No test command was detected"), "{none}");
    }

    #[test]
    fn the_prompt_keeps_the_repository_free_of_plan_files() {
        let prompt = crate::flux::fix_prompt("Export drops a day", "=== TESTS ===\n", None);
        assert!(prompt.contains("Export drops a day"));
        assert!(prompt.contains("regression test"));
        assert!(prompt.contains("PLAN.md"), "the prompt forbids plan files by name");
        assert!(prompt.contains("the FIRST test is the issue exactly as reported"), "{prompt}");
        assert!(prompt.contains("no new guards"), "a minimal fix is asked for: {prompt}");
        assert!(!prompt.contains("PREVIOUS ATTEMPT"));
        let retry = crate::flux::fix_prompt("x", "", Some(&retry_block("`npm test` gave exit 1", "")));
        assert!(retry.contains("PREVIOUS ATTEMPT FAILED") && retry.contains("exit 1"));
    }

    #[tokio::test]
    async fn verification_skips_a_cmd_that_repeats_the_test_command() {
        let dir = tempfile::tempdir().unwrap();
        let job = job_in(dir.path());
        let hooks = EngineHooks::default();
        let runs = verify(&job, &hooks, Some("true"), Some("  true ")).await;
        assert_eq!(runs.len(), 1);
        let runs = verify(&job, &hooks, Some("false"), Some("true")).await;
        assert_eq!(runs.len(), 1, "a failing check stops the rest");
        assert!(!runs[0].ok());
        assert!(verify(&job, &hooks, None, None).await.is_empty());
    }

    /// The rescue a unit test gets: kept in this slot, never in the
    /// process-wide shutdown hooks, so no other test can fire it.
    type Slot = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

    fn no_stop() {}

    fn fire(slot: &Slot) {
        let hook = slot.lock().unwrap().take().expect("a rescue was registered");
        hook();
    }

    /// A committed `value.txt` of 1, then the session's fix (2) and the
    /// test that checks for it.
    fn fixed_repo() -> (tempfile::TempDir, Job, BTreeMap<String, repo::Content>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        std::fs::write(root.join("value.txt"), "1\n").unwrap();
        sh(root, "git add -A && git commit -qm init");
        let job = job_in(root);
        let before = job.git.as_ref().unwrap().snapshot();
        std::fs::write(root.join("value.txt"), "2\n").unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/check.sh"), "grep -qx 2 value.txt\n").unwrap();
        let changed = job.git.as_ref().unwrap().changed_since(&before);
        (dir, job, before, changed)
    }

    fn value(job: &Job) -> String {
        std::fs::read_to_string(job.cwd.join("value.txt")).unwrap()
    }

    /// The core proof: with the fix taken out the new test must fail, and
    /// the fix must be back on disk afterwards either way.
    #[tokio::test]
    async fn reproduction_reverts_the_source_and_puts_the_fix_back() {
        let (_dir, job, before, changed) = fixed_repo();
        let slot = Slot::default();
        let keep = |hook| *slot.lock().unwrap() = Some(hook);
        let rescuer = Rescuer { register: &keep, stop: no_stop };

        let repro = reproduce(&job, &before, &changed, &[], "bash tests/check.sh", TEST_TIMEOUT, None, &rescuer).await.0;
        assert_eq!(repro, Repro::Regression { status: "exit 1".into() });
        assert_eq!(value(&job), "2\n", "fix restored");

        // A rescue that fires after the check finished has nothing to put
        // back, and must not undo work done since.
        std::fs::write(job.cwd.join("value.txt"), "3\n").unwrap();
        fire(&slot);
        assert_eq!(value(&job), "3\n", "a late rescue writes nothing");

        // A test that passes either way proves nothing; a failing baseline
        // is the fallback evidence.
        std::fs::write(job.cwd.join("value.txt"), "2\n").unwrap();
        let weak = reproduce(&job, &before, &changed, &[], "true", TEST_TIMEOUT, None, &rescuer).await.0;
        assert!(matches!(weak, Repro::NotShown(_)), "{weak:?}");
        let base = Run { cmd: "t".into(), code: Some(1), output: String::new(), secs: 0.0, timed_out: false };
        let fallback = reproduce(&job, &before, &changed, &[], "true", TEST_TIMEOUT, Some(&base), &rescuer).await.0;
        assert_eq!(fallback, Repro::Baseline { status: "exit 1".into() });
    }

    /// `old` committed, then the session's `new` on top: the job, the
    /// snapshot, what changed, and the old tests it edited.
    #[allow(clippy::type_complexity)]
    fn session(
        old: &[(&str, &str)],
        new: &[(&str, &str)],
    ) -> (tempfile::TempDir, Job, BTreeMap<String, repo::Content>, Vec<String>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let put = |files: &[(&str, &str)]| {
            for (path, body) in files {
                std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
                std::fs::write(root.join(path), body).unwrap();
            }
        };
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        put(old);
        sh(root, "git add -A && git commit -qm init");
        let job = job_in(root);
        let git = job.git.as_ref().unwrap();
        let before = git.snapshot();
        put(new);
        let changed = git.changed_since(&before);
        let edited = edited_old_tests(git, &before, &changed).into_iter().map(|e| e.path).collect();
        (dir, job, before, changed, edited)
    }

    /// An old expectation that grew a line is a rewrite, inserted or not:
    /// the session's tests without that line fail with the fix, so the
    /// line belongs to an old test and is never the proof.
    #[tokio::test]
    async fn an_old_expectation_that_grew_a_line_is_not_the_reproduction() {
        let check = |lines: &str| format!("diff out.txt - <<'EOF'\n{lines}EOF\n");
        let (_dir, job, before, changed, edited) = session(
            &[("out.txt", "a\nb\n"), ("tests/check.sh", &check("a\nb\n"))],
            &[("out.txt", "A\nb\nc\n"), ("tests/check.sh", &check("A\nb\nc\n"))],
        );
        assert_eq!(edited, ["tests/check.sh"]);
        let rescuer = Rescuer { register: &|_| {}, stop: no_stop };
        let (repro, _) = reproduce(&job, &before, &changed, &edited, "bash tests/check.sh", TEST_TIMEOUT, None, &rescuer).await;
        let Repro::NotShown(why) = &repro else { panic!("{repro:?}") };
        assert!(why.contains("inside tests/check.sh belong to old tests"), "{why}");
        assert_eq!(std::fs::read_to_string(job.cwd.join("tests/check.sh")).unwrap(), check("A\nb\nc\n"));
        assert_eq!(std::fs::read_to_string(job.cwd.join("out.txt")).unwrap(), "A\nb\nc\n");
    }

    /// A new test that needs a name the session added to an old import
    /// line: with that line restored, the test file fails to load, and a
    /// file that does not load proves nothing.
    #[tokio::test]
    async fn new_tests_that_do_not_load_with_the_old_lines_are_not_the_reproduction() {
        let calc = |double: &str| format!("def double(x):\n    return {double}\n\n\ndef half(x):\n    return x // 2\n");
        let test = |import: &str, rest: &str| {
            format!("import unittest\nfrom calc import {import}\n\n\nclass T(unittest.TestCase):\n    def test_three(self):\n{rest}")
        };
        let (_dir, job, before, changed, edited) = session(
            &[("calc.py", &calc("x + 2")), ("tests/test_calc.py", &test("double", "        self.assertEqual(double(3), 5)\n"))],
            &[
                ("calc.py", &calc("x * 2")),
                (
                    "tests/test_calc.py",
                    &test(
                        "double, half",
                        "        self.assertEqual(double(3), 6)\n\n    def test_half(self):\n        self.assertEqual(half(4), 2)\n",
                    ),
                ),
            ],
        );
        let rescuer = Rescuer { register: &|_| {}, stop: no_stop };
        let cmd = "python3 -m unittest discover -s tests";
        let (repro, _) = reproduce(&job, &before, &changed, &edited, cmd, TEST_TIMEOUT, None, &rescuer).await;
        let Repro::NotShown(why) = &repro else { panic!("{repro:?}") };
        assert!(why.contains("inside tests/test_calc.py do not run with the old lines back (NameError)"), "{why}");
    }

    /// The without-the-fix check runs under its own timeout: the largest of
    /// ten times the passing run, three times the baseline run (which paid
    /// the compile the passing run skipped), and a 30s floor, capped at
    /// TEST_TIMEOUT.
    #[test]
    fn the_without_fix_timeout_scales_with_the_passing_and_baseline_runs() {
        let run = |secs: f64| Run { cmd: "t".into(), code: Some(0), output: String::new(), secs, timed_out: false };
        // The floor: both runs fast.
        assert_eq!(repro_timeout(&run(0.5), Some(&run(3.0))), Duration::from_secs(30));
        // The passing run dominates. A slow suite (38s, the pyparsing
        // case) gets 3x, not a ceiling it would always hit.
        assert_eq!(repro_timeout(&run(38.0), Some(&run(3.0))), Duration::from_secs(114));
        // The baseline dominates: the swap's rebuild dwarfs the incremental
        // passing run, and 30s would call a slow finish a hang.
        assert_eq!(repro_timeout(&run(0.5), Some(&run(20.0))), Duration::from_secs(60));
        // The ceiling binds either term.
        assert_eq!(repro_timeout(&run(120.0), Some(&run(3.0))), TEST_TIMEOUT);
        assert_eq!(repro_timeout(&run(10.0), Some(&run(3.0))), Duration::from_secs(30));
        assert_eq!(repro_timeout(&run(0.5), Some(&run(150.0))), TEST_TIMEOUT);
    }

    /// A baseline that never ran or never finished cannot size the rebuild,
    /// so the check gets TEST_TIMEOUT, as it did before the timeout was
    /// sized at all.
    #[test]
    fn the_without_fix_timeout_falls_back_to_the_ceiling_without_a_baseline() {
        let run = |secs: f64, timed_out: bool| Run {
            cmd: "t".into(),
            code: Some(0),
            output: String::new(),
            secs,
            timed_out,
        };
        assert_eq!(repro_timeout(&run(0.5, false), None), TEST_TIMEOUT);
        assert_eq!(repro_timeout(&run(0.5, false), Some(&run(20.0, true))), TEST_TIMEOUT);
    }

    /// A hang bug: the new test never finishes against the original code.
    /// The check ends at its own timeout, not TEST_TIMEOUT, and the
    /// timeout itself is the reproduction.
    #[tokio::test]
    async fn a_check_that_hangs_without_the_fix_is_the_reproduction() {
        let (_dir, job, before, changed) = fixed_repo();
        let slot = Slot::default();
        let keep = |hook| *slot.lock().unwrap() = Some(hook);
        let rescuer = Rescuer { register: &keep, stop: no_stop };

        let hangs = "while grep -qx 1 value.txt; do sleep 0.05; done";
        let clock = std::time::Instant::now();
        let (repro, run) =
            reproduce(&job, &before, &changed, &[], hangs, Duration::from_secs(2), None, &rescuer).await;
        assert_eq!(repro, Repro::Hangs { status: "timed out after 2s".into() });
        assert!(run.unwrap().timed_out);
        assert!(clock.elapsed() < Duration::from_secs(15), "the check ends at its own timeout");
        assert_eq!(value(&job), "2\n", "fix restored");
    }

    /// The race that lost a fix: the rescue fired after it was registered
    /// and before the swap, so it restored nothing, and the swap then wrote
    /// the originals over the fix for good. Now the swap sees the rescue
    /// and never starts.
    #[tokio::test]
    async fn a_rescue_between_register_and_swap_leaves_the_fix_on_disk() {
        let (_dir, job, before, changed) = fixed_repo();
        let fire_at_once = |hook: Box<dyn FnOnce() + Send>| hook();
        let rescuer = Rescuer { register: &fire_at_once, stop: no_stop };

        let repro = reproduce(&job, &before, &changed, &[], "bash tests/check.sh", TEST_TIMEOUT, None, &rescuer).await.0;
        assert_eq!(repro, Repro::NotShown("interrupted".into()));
        assert_eq!(value(&job), "2\n", "the fix is on disk");
    }

    /// A rescue while the check runs puts the fix back, and the end of the
    /// check does not write over it again.
    #[tokio::test]
    async fn a_rescue_during_the_check_puts_the_fix_back() {
        let (_dir, job, before, changed) = fixed_repo();
        let slot = Slot::default();
        let keep = |hook| *slot.lock().unwrap() = Some(hook);
        let rescuer = Rescuer { register: &keep, stop: no_stop };

        let check = "echo seen > seen.txt; while [ ! -f continue.txt ]; do sleep 0.01; done; bash tests/check.sh";
        let (repro, during) = tokio::join!(
            reproduce(&job, &before, &changed, &[], check, TEST_TIMEOUT, None, &rescuer),
            async {
                while !job.cwd.join("seen.txt").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                let during = value(&job);
                fire(&slot);
                std::fs::write(job.cwd.join("continue.txt"), "ok\n").unwrap();
                during
            }
        );
        assert_eq!(during, "1\n", "the originals were on disk during the check");
        assert_eq!(repro.0, Repro::NotShown("interrupted".into()));
        assert_eq!(value(&job), "2\n", "the rescue put the fix back");
    }

    /// The TUI critic's case: `a + b` and `a * b` have the same size, and
    /// Python trusts a `.pyc` whose recorded mtime second and size match
    /// the source. After a swap inside one second the old bytecode ran,
    /// and the suite failed right after slag printed FIXED.
    #[tokio::test]
    async fn a_same_size_same_second_swap_leaves_no_stale_bytecode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let old = "def f(a, b):\n    return a + b\n";
        let new = "def f(a, b):\n    return a * b\n";
        assert_eq!(old.len(), new.len());
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        std::fs::write(root.join("calc.py"), old).unwrap();
        std::fs::write(root.join(".gitignore"), "__pycache__/\n").unwrap();
        sh(root, "git add -A && git commit -qm init");
        let job = job_in(root);
        let before = job.git.as_ref().unwrap().snapshot();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("tests/test_calc.py"),
            "import unittest\nfrom calc import f\n\nclass T(unittest.TestCase):\n    \
             def test_multiplies(self):\n        self.assertEqual(f(2, 3), 6)\n",
        )
        .unwrap();

        // Build the trap on purpose: bytecode of the old source, then the
        // new source in place with the same size and the same mtime.
        let python = "env -u PYTHONDONTWRITEBYTECODE python3";
        let pin = "python3 -c 'import os; os.utime(\"calc.py\", (1700000000, 1700000000))'";
        sh(root, pin);
        sh(root, &format!("{python} -c 'import calc'"));
        std::fs::write(root.join("calc.py"), new).unwrap();
        sh(root, pin);
        let stale = std::process::Command::new("bash")
            .args(["-c", &format!("{python} -c 'import calc; print(calc.f(2, 3))'")])
            .current_dir(root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&stale.stdout).trim(), "5", "the trap is real");

        let changed = job.git.as_ref().unwrap().changed_since(&before);
        let slot = Slot::default();
        let keep = |hook| *slot.lock().unwrap() = Some(hook);
        let rescuer = Rescuer { register: &keep, stop: no_stop };
        let cmd = "python3 -m unittest discover -s tests";
        let (repro, _) = reproduce(&job, &before, &changed, &[], cmd, TEST_TIMEOUT, None, &rescuer).await;
        assert_eq!(repro, Repro::Regression { status: "exit 1".into() });

        // Same second, same size again: only a purged cache recompiles.
        sh(root, pin);
        let after = std::process::Command::new("bash")
            .args(["-c", &format!("{python} -m unittest discover -s tests")])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            after.status.success(),
            "the user's suite must pass right after FIXED: {}",
            String::from_utf8_lossy(&after.stderr)
        );
    }

    /// The critic's case as it happens, with nothing pinned: the
    /// reproduction run compiles the old source (this check command writes
    /// bytecode on purpose), the fix is swapped back at once, and the tree
    /// slag hands over must pass `python3 -m unittest` like the commit.
    #[tokio::test]
    async fn the_handed_over_tree_passes_its_suite_right_after_the_swap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        std::fs::write(root.join("calc.py"), "def f(a, b):\n    return a + b\n").unwrap();
        std::fs::write(root.join(".gitignore"), "__pycache__/\n").unwrap();
        sh(root, "git add -A && git commit -qm init");
        let job = job_in(root);
        let before = job.git.as_ref().unwrap().snapshot();
        std::fs::write(root.join("calc.py"), "def f(a, b):\n    return a * b\n").unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("tests/test_calc.py"),
            "import unittest\nfrom calc import f\n\nclass T(unittest.TestCase):\n    \
             def test_multiplies(self):\n        self.assertEqual(f(2, 3), 6)\n",
        )
        .unwrap();
        let changed = job.git.as_ref().unwrap().changed_since(&before);

        let slot = Slot::default();
        let keep = |hook| *slot.lock().unwrap() = Some(hook);
        let rescuer = Rescuer { register: &keep, stop: no_stop };
        let started = std::time::SystemTime::now();
        let writes_bytecode = "env -u PYTHONDONTWRITEBYTECODE python3 -m unittest discover -s tests";
        let (repro, _) = reproduce(&job, &before, &changed, &[], writes_bytecode, TEST_TIMEOUT, None, &rescuer).await;
        assert_eq!(repro, Repro::Regression { status: "exit 1".into() });

        let secs = |t: std::time::SystemTime| {
            t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        };
        let restored = std::fs::metadata(root.join("calc.py")).unwrap().modified().unwrap();
        assert!(secs(restored) > secs(started), "the fix's mtime second is past the old source's");
        let suite = std::process::Command::new("bash")
            .args(["-c", "env -u PYTHONDONTWRITEBYTECODE python3 -m unittest discover -s tests"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            suite.status.success(),
            "the handed-over tree must pass like the commit: {}",
            String::from_utf8_lossy(&suite.stderr)
        );
    }

    #[test]
    fn swap_mtimes_never_share_a_second() {
        let secs = |t: std::time::SystemTime| {
            t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        };
        let path = std::path::Path::new("/nonexistent/slag/swap.py");
        let (a, b, c) = (swap_mtime(path), swap_mtime(path), swap_mtime(path));
        assert!(secs(a) < secs(b) && secs(b) < secs(c), "{a:?} {b:?} {c:?}");
        let other = swap_mtime(std::path::Path::new("/nonexistent/slag/other.py"));
        assert!(secs(other) <= secs(a) + 1, "another file does not inherit this one's drift");
    }

    #[test]
    fn stray_files_from_a_broken_command_never_reach_the_commit() {
        let (_dir, job, before, _) = fixed_repo();
        let git = job.git.as_ref().unwrap();
        for junk in ["-t", "202601011200", "result.log"] {
            std::fs::write(job.cwd.join(junk), "").unwrap();
        }
        let mut changed = git.changed_since(&before);
        let mut notes = Vec::new();
        sweep_debris(git, &before, &mut changed, &mut notes, &EngineHooks::default());
        assert_eq!(changed, ["value.txt", "tests/check.sh"], "{changed:?}");
        assert!(!job.cwd.join("-t").exists() && !job.cwd.join("202601011200").exists());
        assert!(notes[0].contains("-t") && notes[0].contains("202601011200"), "{notes:?}");
        assert!(notes[0].contains("result.log") && !job.cwd.join("result.log").exists(), "{notes:?}");
    }

    #[test]
    fn rewritten_or_deleted_old_tests_are_found_and_additions_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let pin = "test('preview', () => {\n  assert.equal(makePreview(s, 10), 'hello worl…');\n});\n";
        std::fs::write(root.join("tests/preview.test.js"), pin).unwrap();
        std::fs::write(root.join("tests/extra.test.js"), "test('x', () => {});\n").unwrap();
        std::fs::write(root.join("tests/gone.test.js"), "test('gone', () => {});\n").unwrap();
        let lib = "pub fn f() -> u8 { 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn missing_value_is_an_error() {}\n}\n";
        std::fs::write(root.join("src/lib.rs"), lib).unwrap();
        sh(root, "git add -A && git commit -qm init");
        let git = Git::open(root).unwrap();
        let before = git.snapshot();

        // js-notify-preview: an expectation rewritten to fit the fix.
        std::fs::write(root.join("tests/preview.test.js"), pin.replace("hello worl…", "hello wor…")).unwrap();
        // An addition beside the old tests is what a regression test is.
        std::fs::write(root.join("tests/extra.test.js"), "test('x', () => {});\ntest('y', () => {});\n").unwrap();
        // A pre-existing test file deleted outright.
        std::fs::remove_file(root.join("tests/gone.test.js")).unwrap();
        // rust-hawkconf: an inline test deleted beside a source change.
        let new_lib = "pub fn f() -> u8 { 2 }\n#[cfg(test)]\nmod tests {\n}\n";
        std::fs::write(root.join("src/lib.rs"), new_lib).unwrap();

        let changed = git.changed_since(&before);
        let edited = edited_old_tests(&git, &before, &changed);
        let paths: Vec<&str> = edited.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["src/lib.rs", "tests/gone.test.js", "tests/preview.test.js"], "{edited:?}");
        let preview = edited.iter().find(|e| e.path == "tests/preview.test.js").unwrap();
        assert_eq!(preview.removed, ["assert.equal(makePreview(s, 10), 'hello worl…');"]);
        assert_eq!(preview.original_tests.as_deref(), Some(pin.as_bytes()));
        let rust = edited.iter().find(|e| e.path == "src/lib.rs").unwrap();
        let hybrid = String::from_utf8(rust.original_tests.clone().unwrap()).unwrap();
        assert!(hybrid.starts_with("pub fn f() -> u8 { 2 }"), "the fixed code: {hybrid}");
        assert!(hybrid.contains("fn missing_value_is_an_error"), "with the original tests: {hybrid}");
    }

    #[test]
    fn a_change_to_old_tests_counts_as_declared_only_when_every_file_is_named() {
        let edited = |path: &str| EditedTest { path: path.into(), removed: vec![], diff: String::new(), original_tests: None };
        let both = [edited("tests/preview.test.js"), edited("tests/limits.test.js")];
        let said = "Fixed the slice.\nchanged existing tests: tests/preview.test.js: the issue asks for 20 code points";
        assert!(declaration(said, &both).is_none(), "limits.test.js went unsaid");
        let all = format!("{said}\nChanged existing tests: limits.test.js: same ask");
        let decl = declaration(&all, &both).expect("both named");
        assert!(decl.contains("preview.test.js") && decl.contains("limits.test.js"), "{decl}");
        assert!(declaration("Fixed it.", &both[..1]).is_none());
    }

    #[test]
    fn a_review_pass_needs_the_word_and_nothing_else_counts() {
        assert!(matches!(read_review("VERDICT: pass\nGAP:"), Review::Pass(_)));
        assert!(matches!(read_review("VERDICT: fail\nGAP: add the boundary"), Review::Gap(g) if g == "add the boundary"));
        assert!(matches!(read_review("Looks plausible, I think."), Review::Unclear));
        assert!(matches!(read_review("VERDICT: fail\nGAP:"), Review::Unclear));
    }

    #[test]
    fn a_test_edit_may_not_collapse_many_checks_into_fewer() {
        let issue = "totals are wrong";
        let old: String = (0..40).map(|i| format!("    assert_eq!(total({i}), {i});\n")).collect();
        let edit = |new: &str| EditedTest {
            path: "tests/t.rs".into(),
            removed: vec![],
            diff: similar::TextDiff::from_lines(old.as_str(), new).unified_diff().to_string(),
            original_tests: Some(old.as_bytes().to_vec()),
        };
        let answer = |e: &EditedTest| {
            let removed = e.diff.lines().find_map(|l| l.strip_prefix('-').filter(|_| !l.starts_with("---"))).unwrap();
            let added = e.diff.lines().find_map(|l| l.strip_prefix('+').filter(|_| !l.starts_with("+++"))).unwrap();
            let row = serde_json::json!([{
                "path": "tests/t.rs",
                "removed_behavior_wrong": "yes",
                "added_behavior_requested": "yes",
                "preserves_checks": "yes",
                "issue_quote": issue,
                "removed_quote": removed.trim(),
                "added_quote": added.trim(),
            }]);
            format!("VERDICT: pass\nGAP:\nTEST_EDITS: {row}")
        };
        let collapsed = edit("    assert!(true);\n");
        let err = agree_test_edits(&answer(&collapsed), std::slice::from_ref(&collapsed), issue).unwrap_err();
        assert!(err.contains("40 check lines to 1"), "{err}");

        let corrected = edit(&old.replace("total(0), 0", "total(0), 1"));
        assert!(agree_test_edits(&answer(&corrected), std::slice::from_ref(&corrected), issue).is_ok());
    }

    #[test]
    fn a_removed_comment_is_not_a_dropped_check() {
        // The docutils fix deletes an outdated TODO that says "should".
        let diff = "--- a/t.py\n+++ b/t.py\n-# TODO: Anonymous links to a \"clickable image should go\n-        <reference refuri=\"x\">\n+        <reference refid=\"y\">\n";
        assert_eq!(check_lines(diff, '-'), 0);
        let collapse = "-    assert a == 1\n-    assert_eq!(b, 2);\n-    expect(c).toBe(3)\n+    assert True\n";
        assert_eq!(check_lines(collapse, '-'), 3);
        assert_eq!(check_lines(collapse, '+'), 1);
        assert_eq!(check_lines("+    // checks elsewhere\n+    if t.Error(x) {}\n", '+'), 1);
    }

    #[test]
    fn test_edit_agreement_needs_grounded_yes_answers_for_each_path() {
        let issue = "link() returns https://target, which is wrong; it must return #image.";
        let old = "    assert_eq!(link(), \"https://target\");\n";
        let new = "    assert_eq!(link(), \"#image\");\n";
        let edit = EditedTest {
            path: "tests/links.rs".into(),
            removed: vec![old.trim().into()],
            diff: similar::TextDiff::from_lines(old, new).unified_diff().to_string(),
            original_tests: Some(old.as_bytes().to_vec()),
        };
        let row = serde_json::json!({
            "path": "tests/links.rs",
            "removed_behavior_wrong": "yes",
            "added_behavior_requested": "yes",
            "preserves_checks": "yes",
            "issue_quote": issue,
            "removed_quote": old.trim(),
            "added_quote": new.trim(),
        });
        let agree = |rows: serde_json::Value, edits: &[EditedTest]| {
            agree_test_edits(&format!("VERDICT: pass\nGAP:\nTEST_EDITS: {rows}"), edits, issue)
        };
        assert!(agree(serde_json::json!([row]), std::slice::from_ref(&edit)).is_ok());
        for (field, value) in [
            ("removed_behavior_wrong", "no"),
            ("added_behavior_requested", "no"),
            ("preserves_checks", "no"),
            ("issue_quote", "the issue never said this"),
            ("removed_quote", "assert_eq!(link(), \"other\");"),
            ("added_quote", "assert_eq!(link(), \"other\");"),
            ("added_quote", "#image"),
            ("path", "tests/other.rs"),
        ] {
            let mut bad = row.clone();
            bad[field] = value.into();
            assert!(agree(serde_json::json!([bad]), std::slice::from_ref(&edit)).is_err(), "{field}");
        }
        assert!(agree(serde_json::json!([]), std::slice::from_ref(&edit)).is_err());
        assert!(agree_test_edits("VERDICT: pass\nGAP:", std::slice::from_ref(&edit), issue).is_err());
        assert!(agree_test_edits("TEST_EDITS: invalid", std::slice::from_ref(&edit), issue).is_err());
        assert!(agree_test_edits(
            "VERDICT: pass\nGAP:", std::slice::from_ref(&edit),
            "tests/links.rs must change neither its scope nor unrelated expectations.",
        ).is_err());
        for unrelated in [
            "Neither tests/links.rs nor tests/other.rs must change.",
            "tests/links.rsx must change.",
        ] {
            assert!(agree_test_edits("VERDICT: pass\nGAP:", std::slice::from_ref(&edit), unrelated).is_err());
        }
        let mut deleted = edit.clone();
        deleted.diff = similar::TextDiff::from_lines(old, "").unified_diff().to_string();
        assert!(agree_test_edits(
            "VERDICT: pass\nGAP:", &[deleted], "tests/links.rs pins the old value and must change.",
        ).is_err());
        let mut weakened = edit.clone();
        weakened.diff = similar::TextDiff::from_lines(old, "    // Checked elsewhere.\n").unified_diff().to_string();
        assert!(agree_test_edits(
            "VERDICT: pass\nGAP:", &[weakened], "tests/links.rs pins the old value and must change.",
        ).is_err());
        let answer = format!("TEST_EDITS: {}", serde_json::json!([row]));
        assert!(agree_test_edits(&format!("{answer}\n{answer}"), std::slice::from_ref(&edit), issue).is_err());
        for field in ["issue_quote", "removed_quote", "added_quote", "preserves_checks"] {
            let mut missing = row.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(agree(serde_json::json!([missing]), std::slice::from_ref(&edit)).is_err(), "{field}");
            let mut empty = row.clone();
            empty[field] = "".into();
            assert!(agree(serde_json::json!([empty]), std::slice::from_ref(&edit)).is_err(), "{field}");
        }
        let mut other = edit.clone();
        other.path = "tests/other.rs".into();
        let edits = [edit, other];
        assert!(agree(serde_json::json!([row]), &edits).is_err());
        assert!(agree(serde_json::json!([row, row]), &edits).is_err());
        let mut second = row.clone();
        second["path"] = "tests/other.rs".into();
        assert!(agree(serde_json::json!([row, second]), &edits).is_ok());
    }

    #[test]
    fn edited_test_diffs_keep_all_changed_lines_and_their_indentation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        std::fs::create_dir_all(root.join("tests")).unwrap();
        let old = (0..40).map(|i| format!("    assert_eq!(f({i}), 1);\n")).collect::<String>();
        std::fs::write(root.join("tests/old.rs"), &old).unwrap();
        let inline = format!("fn f() {{}}\n#[cfg(test)]\nmod tests {{\n{old}}}\n");
        std::fs::write(root.join("lib.rs"), &inline).unwrap();
        sh(root, "git add -A && git commit -qm init");
        let git = Git::open(root).unwrap();
        let before = git.snapshot();
        std::fs::write(root.join("tests/old.rs"), old.replace(", 1)", ", 2)")).unwrap();
        std::fs::write(root.join("lib.rs"), inline.replace("fn f() {}", "fn f() { return; }").replace(", 1)", ", 2)")).unwrap();
        let edits = edited_old_tests(&git, &before, &git.changed_since(&before));
        assert_eq!(edits.len(), 2);
        for e in edits {
            assert!(e.diff.contains("-    assert_eq!(f(0), 1);"), "{}", e.diff);
            assert!(e.diff.contains("+    assert_eq!(f(39), 2);"), "{}", e.diff);
            assert!(!e.diff.contains("fn f()"), "only test changes: {}", e.diff);
        }
    }

    #[test]
    fn a_declared_test_change_must_share_a_name_with_the_issue() {
        let edited = |line: &str| vec![EditedTest {
            path: "tests/preview.test.js".into(),
            removed: vec![line.into()],
            diff: String::new(),
            original_tests: None,
        }];
        let issue = "makePreview(\"🎉\".repeat(25), 20) splits an emoji";
        assert!(ties_to_issue(&edited("assert.equal(makePreview(s, 20), x);"), issue));
        assert!(!ties_to_issue(&edited("assert.equal(f('hello worl…'), 1);"), issue), "nothing the issue names");
        assert!(!ties_to_issue(&edited("expect(x).toBe(1);"), "use 1 thing"), "single digits and test words do not tie");
    }

    #[test]
    fn only_an_explicit_fail_with_a_named_gap_sends_a_fix_back() {
        let fail = "Looks off.\nVERDICT: fail\nGAP: move the test to tests/test_password.py";
        assert_eq!(review_gap(fail).as_deref(), Some("move the test to tests/test_password.py"));
        assert_eq!(review_gap("VERDICT: pass\nGAP:"), None);
        assert_eq!(review_gap("VERDICT: fail\nGAP:"), None, "a fail with no gap is not actionable");
        assert_eq!(review_gap("I think it is fine."), None, "garble lets the verified fix stand");
        assert_eq!(review_gap(""), None);
    }

    #[test]
    fn the_revision_brief_names_the_gap_and_starts_from_the_passing_change() {
        let block = revision_block("assert export_csv on 2024-01-01..03", "+fixed line");
        let prompt = crate::flux::fix_prompt("issue", "", Some(&block));
        assert!(prompt.contains("REVIEW: ONE GAP LEFT"), "{prompt}");
        assert!(prompt.contains("assert export_csv on 2024-01-01..03") && prompt.contains("+fixed line"));
        assert!(!prompt.contains("PREVIOUS ATTEMPT FAILED"), "a passing fix is not a failure");
        let review = crate::flux::fix_review_prompt("the issue", "the diff", "tests/a.py", "exit 1", "untouched", repo::platform(), true);
        assert!(!review.contains("EXISTING BEHAVIOR"), "only offered when old tests changed");
        assert!(review.contains("Never ask the author to restore the behavior the issue reports"));
        let declared = crate::flux::fix_review_prompt("i", "d", "t", "w", "The fix rewrites lines", "p", true);
        assert!(declared.contains("EXISTING BEHAVIOR"), "{declared}");
        for part in ["ROOT CAUSE", "THE ISSUE'S CASES", "PLACE", "VALID", "VERDICT: pass or fail", "the diff"] {
            assert!(review.contains(part), "{part} missing");
        }
    }

    /// The pycparser shape: a fix and a test for the literal example pass
    /// their own proof and still miss the same bug one statement form over.
    #[test]
    fn the_brief_and_the_review_ask_for_the_bug_class_not_only_the_example() {
        let prompt = crate::flux::fix_prompt("issue", "", None);
        assert!(prompt.contains("one example of a bug class"), "{prompt}");
        assert!(prompt.contains("the syntax or statement form around it, nesting"), "{prompt}");
        assert!(prompt.contains("into the regression test (a loop or table over the inputs)"), "{prompt}");
        assert!(prompt.contains("the variants of step 4"), "{prompt}");
        let review = crate::flux::fix_review_prompt("i", "d", "t", "w", "untouched", "p", true);
        assert!(review.contains("5. BUG CLASS"), "{review}");
        assert!(review.contains("name one concrete variant the tests do not try"), "{review}");
        assert!(!review.contains("never mentions, misses"), "a variant test is no stray test: {review}");
        let agreement = crate::flux::fix_review_prompt("i", "d", "t", "w", "The fix rewrites lines", "p", false);
        assert!(!agreement.contains("BUG CLASS"), "{agreement}");
        // A named variant is an ordinary gap, so it gets the one revision.
        let raw = "VERDICT: fail\nGAP: add `if (TT) TT++;` as the loop body in tests/test_c_parser.py";
        assert!(matches!(read_review(raw), Review::Gap(g) if g.contains("if (TT) TT++;")));
    }

    #[test]
    fn deleting_or_skipping_an_existing_test_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        std::fs::create_dir_all(root.join("tests")).unwrap();
        let suite = "def test_a():\n    pass\n\ndef test_b():\n    pass\n";
        std::fs::write(root.join("tests/test_x.py"), suite).unwrap();
        sh(root, "git add -A && git commit -qm init");
        let git = Git::open(root).unwrap();
        let before = git.snapshot();
        let changed = vec!["tests/test_x.py".to_string()];

        std::fs::write(root.join("tests/test_x.py"), format!("{suite}\ndef test_new():\n    pass\n")).unwrap();
        assert_eq!(lost_tests(&git, &before, &changed), None, "adding a test is the point");

        std::fs::write(root.join("tests/test_x.py"), "def test_a():\n    pass\n").unwrap();
        let lost = lost_tests(&git, &before, &changed).expect("a deleted test is caught");
        assert!(lost.contains("tests/test_x.py (2 tests, 0 skipped -> 1, 0)"), "{lost}");

        std::fs::write(root.join("tests/test_x.py"), format!("import pytest\n@pytest.mark.skip\n{suite}")).unwrap();
        assert!(lost_tests(&git, &before, &changed).is_some(), "a skipped test is caught");

        // Rust keeps unit tests in the source file itself.
        std::fs::write(root.join("tests/test_x.py"), suite).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let lib = "pub fn f() {}\n#[cfg(test)]\nmod t {\n    #[test]\n    fn a() {}\n}\n";
        std::fs::write(root.join("src/lib.rs"), lib).unwrap();
        sh(root, "git add -A && git commit -qm lib");
        let before = git.snapshot();
        std::fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        let lost = lost_tests(&git, &before, &["src/lib.rs".to_string()]);
        assert!(lost.is_some_and(|l| l.contains("src/lib.rs")), "an inline test is caught");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_script_keeps_its_mode_through_the_reproduction() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sh(root, "git init -q -b main && git config user.email t@t && git config user.name t");
        std::fs::write(root.join("value.txt"), "1\n").unwrap();
        sh(root, "git add -A && git commit -qm init");
        let job = job_in(root);
        let git = job.git.as_ref().unwrap();
        let before = git.snapshot();

        std::fs::write(root.join("run.sh"), "#!/bin/sh\necho 2\n").unwrap();
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/check.sh"), "test -x run.sh\n").unwrap();
        let changed = git.changed_since(&before);

        let slot = Slot::default();
        let keep = |hook| *slot.lock().unwrap() = Some(hook);
        let rescuer = Rescuer { register: &keep, stop: no_stop };
        let repro = reproduce(&job, &before, &changed, &[], "bash tests/check.sh", TEST_TIMEOUT, None, &rescuer).await.0;
        assert_eq!(repro, Repro::Regression { status: "exit 1".into() });
        let mode = std::fs::metadata(root.join("run.sh")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the restored script is still executable");
    }

    #[test]
    fn a_github_fix_closes_its_issue_in_the_commit() {
        let out = Outcome {
            issue: Issue {
                title: "Crash on empty input".into(),
                body: "b".into(),
                number: Some(7),
                url: None,
                repo: None,
            },
            verdict: Verdict::Fixed,
            attempts: 1,
            checks: vec![Run { cmd: "npm test".into(), code: Some(0), output: "ok".into(), secs: 1.0, timed_out: false }],
            repro: Repro::Regression { status: "exit 1".into() },
            stat: vec![],
            summary: "Root cause: a.js:3".into(),
            branch: None,
            commit: None,
            pr: None,
            notes: vec![],
            files: vec!["src/a.js".into(), "tests/a.test.js".into()],
            phases: Phases::default(),
            time: None,
            spend: None,
            evolve: None,
        };
        let msg = commit_message(&out);
        assert!(msg.starts_with("fix: Crash on empty input\n"), "{msg}");
        assert!(msg.contains("Verified: `npm test` exit 0") && msg.contains("Fixes #7"), "{msg}");
        let body = pr_body(&out);
        assert!(body.contains("## Reproduction") && body.contains("new test catches the bug"), "{body}");
        assert!(body.contains("## Verification") && body.contains("`npm test`: exit 0"), "{body}");
        assert!(body.contains("- `src/a.js`\n- `tests/a.test.js`"), "the PR lists the committed paths: {body}");
    }
}
