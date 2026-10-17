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

mod issue;
mod repo;

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
/// The longest a review may take before the verified fix lands without it.
const REVIEW_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// The test command failed before any edit and passes now.
    Baseline { status: String },
    NotShown(String),
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
    /// Where the wall time went, phase by phase, so waste is visible.
    phases: Phases,
    /// Run time and spend, taken when the fix ends rather than when the
    /// report prints: a dashboard left open must not inflate the wall.
    time: Option<String>,
    spend: Option<String>,
}

/// Seconds per phase of one fix, summed across attempts.
#[derive(Debug, Default, Clone, Copy)]
struct Phases {
    gather: f64,
    session: f64,
    verify: f64,
    repro: f64,
    review: f64,
    land: f64,
}

impl Phases {
    fn line(&self) -> String {
        format!(
            "gather {:.1}s · session {:.1}s · verify {:.1}s · reproduce {:.1}s · review {:.1}s · \
             commit {:.1}s",
            self.gather, self.session, self.verify, self.repro, self.review, self.land
        )
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
    tui::status_line("░", tui::COLD, &format!("issue: {}", tui::truncate(&issue.title, 70)));

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
    let context =
        context_block(job, &issue, &files, &test_files, test.as_ref(), baseline.as_ref());
    let ctx = Ctx {
        job,
        config,
        hooks,
        issue: &issue,
        context: &context,
        before: &before,
        usable_test: usable_test.as_deref(),
    };
    let tries = job.args.tries.max(1);

    let mut failure: Option<Failure> = None;
    let mut checks: Vec<Run> = Vec::new();
    let mut summary = String::new();
    let mut changed: Vec<String> = Vec::new();
    let mut passed = false;
    let mut attempts = 0u8;

    for attempt in 1..=tries {
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
            Attempted::Passed { checks: c, summary: s, changed: ch } => {
                (checks, summary, changed) = (c, s, ch);
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
        phases,
        time: None,
        spend: None,
    };
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
        stamp(&mut out);
        return Ok(out);
    }

    if let Some(git) = &job.git {
        sweep_debris(git, &before, &mut changed, &mut out.notes, hooks);
    }
    let rescuer = Rescuer { register: &register_shutdown, stop: repo::kill_running };
    let clock = std::time::Instant::now();
    let proof_cmd = out.checks[0].cmd.clone();
    let (mut repro, mut without) =
        reproduce(job, &before, &changed, &proof_cmd, baseline.as_ref(), &rescuer).await;
    out.phases.repro += clock.elapsed().as_secs_f64();
    note_repro(hooks, &repro, &proof_cmd);

    // A green fix can still lose to a careful human: a symptom patch, a
    // test that does not encode the reported case, a test in the wrong
    // file, a test that cannot run here. One gap, one chance to close it.
    let clock = std::time::Instant::now();
    let gap = ctx.quality_gap(&changed, &test_files, &repro, without.as_ref()).await?;
    out.phases.review += clock.elapsed().as_secs_f64();
    if let (Some(gap), Some(git)) = (gap, job.git.as_ref()) {
        note(hooks, &format!("review: {gap}"), false);
        let kept = keep(git, &changed);
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
            Attempted::Passed { checks: c, summary: s, changed: ch } => {
                (out.checks, out.summary, changed) = (c, s, ch);
                sweep_debris(git, &before, &mut changed, &mut out.notes, hooks);
                let proof_cmd = out.checks[0].cmd.clone();
                let clock = std::time::Instant::now();
                (repro, without) =
                    reproduce(job, &before, &changed, &proof_cmd, baseline.as_ref(), &rescuer)
                        .await;
                out.phases.repro += clock.elapsed().as_secs_f64();
                note_repro(hooks, &repro, &proof_cmd);
                out.notes.push(format!("revised once after review: {gap}"));
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
    drop(without);
    out.repro = repro;
    emit(&hooks.events, EngineEvent::IngotDone { id: FIX_ID.into(), ok: true });

    let clock = std::time::Instant::now();
    land(job, &mut out, &changed);
    out.phases.land = clock.elapsed().as_secs_f64();
    stamp(&mut out);
    Ok(out)
}

fn note_repro(hooks: &EngineHooks, repro: &Repro, cmd: &str) {
    match repro {
        Repro::NotShown(why) => note(hooks, &format!("reproduction not shown: {why}"), false),
        Repro::Regression { status } => {
            note(hooks, &format!("reproduced: without the fix `{cmd}` gives {status}"), true)
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
}

/// One session, judged by slag.
enum Attempted {
    /// The change passed every check slag ran.
    Passed { checks: Vec<Run>, summary: String, changed: Vec<String> },
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
        let smith = crate::smith::make_smith(self.config, "default", grade, hooks);
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
        let agent_cmd = crate::proof::extract_cmd(&response);
        let summary = strip_cmd_lines(&response);
        let changed = self.changed_now();

        if let Some(git) = &job.git {
            if changed.is_empty() {
                note(hooks, &format!("{label}: no file changed"), false);
                return fail(
                    Failure::new(
                        "the session changed no file",
                        "The session finished without changing any file.",
                    ),
                    summary,
                );
            }
            // A suite goes green easily when tests disappear. The rule is
            // in the prompt; this is the check that it held.
            if let Some(lost) = lost_tests(git, self.before, &changed) {
                note(hooks, &format!("{label}: {lost}"), false);
                return fail(
                    Failure::new(
                        lost.clone(),
                        format!(
                            "The session {lost}. Restore every existing test exactly as it \
                             was, unskipped, and add new tests beside them."
                        ),
                    ),
                    summary,
                );
            }
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
        Ok(Attempted::Passed { checks, summary, changed })
    }

    /// The one gap worth a revision, if any. Two are certain and cost no
    /// model call: no test was added, or the new test passes without the
    /// fix. Past those, one tool-free review call on a fresh context reads
    /// the issue, the diff, and what the tests did with the fix reverted.
    async fn quality_gap(
        &self,
        changed: &[String],
        test_files: &[String],
        repro: &Repro,
        without: Option<&Run>,
    ) -> Result<Option<String>, SlagError> {
        let Some(git) = &self.job.git else { return Ok(None) };
        // A changed test file, or more test definitions anywhere: Rust
        // keeps unit tests inside the source they test.
        let added_test = changed.iter().any(|p| {
            let count = |text: Option<Vec<u8>>| {
                text.map_or(0, |b| repo::test_marks(&String::from_utf8_lossy(&b)).0)
            };
            repo::is_test_path(p)
                || count(std::fs::read(git.top.join(p)).ok()) > count(git.original(self.before, p))
        });
        let has_tests = !test_files.is_empty() || self.usable_test.is_some();
        if !added_test && has_tests {
            return Ok(Some(
                "no regression test was added: add one that asserts the issue's reported \
                 cases, in the test file for the module you fixed, or in a new file named \
                 after it"
                    .into(),
            ));
        }
        // Checked on the run itself, not the verdict: a failing baseline
        // proves the bug, and says nothing about the new test.
        if without.is_some_and(|run| run.ok()) {
            return Ok(Some(
                "the new test also passes with the source change reverted, so it does not \
                 catch the bug: make it assert the exact result the issue reports"
                    .into(),
            ));
        }

        let without_fix = match (repro, without) {
            (Repro::Regression { .. }, Some(run)) => format!(
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
        let prompt = crate::flux::fix_review_prompt(
            &self.issue.render(),
            &clip(&self.diff(changed), REVIEW_DIFF_BYTES),
            &listed,
            &without_fix,
            repo::platform(),
        );
        match review(self.config, self.hooks, &prompt).await {
            Ok(Some(gap)) => Ok(Some(gap)),
            Ok(None) => {
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
}

/// One tool-free call to the judge model. `Some(gap)` on a clear fail with
/// a gap to act on; `None` on a pass, and on a reply too garbled to act on.
async fn review(
    config: &EngineConfig,
    hooks: &EngineHooks,
    prompt: &str,
) -> Result<Option<String>, SlagError> {
    use crate::engine::Provider;
    let provider = crate::engine::agent::SpendTracked::new(
        crate::engine::provider::OpenRouter::with_base_url(
            config.api_key.clone(),
            config.base_url.clone(),
        ),
        crate::engine::agent::SpendAccum::default(),
    );
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
    Ok(review_gap(&resp.content))
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
/// behind). Only new files qualify; nothing the repository had is touched.
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
            (name.starts_with('-') || !name.chars().any(|c| c.is_ascii_alphabetic()))
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
/// that, a test command that failed before the edit and passes after.
async fn reproduce(
    job: &Job,
    before: &BTreeMap<String, repo::Content>,
    changed: &[String],
    cmd: &str,
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

    // Swap the source files back to their pre-session content, run, swap
    // the fix back in. A Ctrl-C at any point must not cost the user the
    // fix, so a rescue is registered first, and every swap and the rescue
    // take one lock: see `Swap`.
    let fixed: Vec<Saved> = sources
        .iter()
        .map(|p| {
            let path = git.top.join(p);
            let mode = std::fs::metadata(&path).ok().map(|m| m.permissions());
            let now = std::fs::read(&path).ok();
            (path, now, mode)
        })
        .collect();
    let swap = Arc::new(Mutex::new(Swap { phase: Phase::Armed, fixed }));
    let rescue = swap.clone();
    let stop = rescuer.stop;
    (rescuer.register)(Box::new(move || Swap::rescue(&rescue, stop)));
    let originals: Vec<Saved> = sources
        .iter()
        .map(|p| (git.top.join(p), git.original(before, p), None))
        .collect();
    let interrupted = || Repro::NotShown("interrupted".into());
    {
        let mut s = swap.lock().unwrap_or_else(|e| e.into_inner());
        if s.phase != Phase::Armed {
            return (interrupted(), None);
        }
        restore(&originals);
        s.phase = Phase::Swapped;
    }
    let run = repo::run(cmd, &job.cwd, TEST_TIMEOUT).await;
    {
        let mut s = swap.lock().unwrap_or_else(|e| e.into_inner());
        if s.phase != Phase::Swapped {
            // The rescue ran during the check and already put the fix back.
            return (interrupted(), None);
        }
        restore(&s.fixed);
        s.phase = Phase::Done;
    }

    let repro = if run.unusable() {
        from_baseline()
            .unwrap_or_else(|| Repro::NotShown(format!("the check without the fix {}", run.status())))
    } else if run.ok() {
        from_baseline()
            .unwrap_or_else(|| Repro::NotShown("the new test also passes without the fix".into()))
    } else {
        Repro::Regression { status: run.status() }
    };
    (repro, Some(run))
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
fn land(job: &Job, out: &mut Outcome, changed: &[String]) {
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
        (Some(t), Some(b)) => s.push_str(&format!(
            "Test command (from {}): {}\nslag ran it before any edit: {} in {:.1}s. The end of \
             its output:\n{}\n",
            t.source,
            t.cmd,
            b.status(),
            b.secs,
            repo::tail(&b.output, OUTPUT_TAIL_LINES, OUTPUT_TAIL_BYTES)
        )),
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

/// The report card's lines under its header. Every glyph is painted on
/// its own and every value clipped as plain text, so no escape sequence
/// is ever cut in half on its way to the screen.
fn report_lines(out: &Outcome, w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let label = |name: &str| tui::dim(&format!("{name:<6}"));
    let row = |name: &str, value: &str| format!("  {}  {}", label(name), tui::clip(value, w.saturating_sub(10)));
    let title = match out.issue.number {
        Some(n) => format!("#{n} {}", out.issue.title),
        None => out.issue.title.clone(),
    };
    let attempts = plural(out.attempts, "attempt");

    // The verdict, alone on its line.
    match &out.verdict {
        Verdict::Fixed => {
            let head = format!("  {} {}  ", tui::paint(tui::PURE, "✓"), tui::paint(tui::PURE, &format!("{}FIXED{}", bold(), reset())));
            let rest = format!("{title} · {attempts}");
            lines.push(format!("{head}{}", tui::clip(&rest, w.saturating_sub(tui::visible_len(&head)))));
        }
        Verdict::Failed(reason) => {
            let head = format!("  {} {}  ", tui::paint(tui::WARM, "✗"), tui::paint(tui::WARM, &format!("{}NOT FIXED{}", bold(), reset())));
            lines.push(format!("{head}{}", tui::clip(reason, w.saturating_sub(tui::visible_len(&head)))));
            lines.push(row("issue", &title));
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
            tui::spread(&left, &tui::paint(if ok { tui::COLD } else { tui::WARM }, meta), w)
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
        lines.push(row(if i == 0 { "diff" } else { "" }, line.trim()));
    }
    let mut work = Vec::new();
    if let (Some(branch), Some(sha)) = (&out.branch, &out.commit) {
        work.push(format!("{branch} @ {sha}"));
    }
    if let Some(pr) = &out.pr {
        work.push(pr.clone());
    }
    if !work.is_empty() {
        lines.push(row("work", &work.join(" · ")));
    }
    for n in &out.notes {
        lines.push(row("note", n));
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
        lines.push(row("run", &took.join(" · ")));
    }
    lines.push(row("", &out.phases.line()));
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

        let repro = reproduce(&job, &before, &changed, "bash tests/check.sh", None, &rescuer).await.0;
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
        let weak = reproduce(&job, &before, &changed, "true", None, &rescuer).await.0;
        assert!(matches!(weak, Repro::NotShown(_)), "{weak:?}");
        let base = Run { cmd: "t".into(), code: Some(1), output: String::new(), secs: 0.0, timed_out: false };
        let fallback = reproduce(&job, &before, &changed, "true", Some(&base), &rescuer).await.0;
        assert_eq!(fallback, Repro::Baseline { status: "exit 1".into() });
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

        let repro = reproduce(&job, &before, &changed, "bash tests/check.sh", None, &rescuer).await.0;
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

        let check = "echo seen > seen.txt; sleep 1; bash tests/check.sh";
        let (repro, during) = tokio::join!(
            reproduce(&job, &before, &changed, check, None, &rescuer),
            async {
                while !job.cwd.join("seen.txt").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                let during = value(&job);
                fire(&slot);
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
        let (repro, _) = reproduce(&job, &before, &changed, cmd, None, &rescuer).await;
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
        let (repro, _) = reproduce(&job, &before, &changed, writes_bytecode, None, &rescuer).await;
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
        for junk in ["-t", "202601011200"] {
            std::fs::write(job.cwd.join(junk), "").unwrap();
        }
        let mut changed = git.changed_since(&before);
        let mut notes = Vec::new();
        sweep_debris(git, &before, &mut changed, &mut notes, &EngineHooks::default());
        assert_eq!(changed, ["value.txt", "tests/check.sh"], "{changed:?}");
        assert!(!job.cwd.join("-t").exists() && !job.cwd.join("202601011200").exists());
        assert!(notes[0].contains("-t") && notes[0].contains("202601011200"), "{notes:?}");
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
        let review = crate::flux::fix_review_prompt("the issue", "the diff", "tests/a.py", "exit 1", repo::platform());
        for part in ["ROOT CAUSE", "THE ISSUE'S CASES", "PLACE", "VALID", "VERDICT: pass or fail", "the diff"] {
            assert!(review.contains(part), "{part} missing");
        }
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
        let repro = reproduce(&job, &before, &changed, "bash tests/check.sh", None, &rescuer).await.0;
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
            phases: Phases::default(),
            time: None,
            spend: None,
        };
        let msg = commit_message(&out);
        assert!(msg.starts_with("fix: Crash on empty input\n"), "{msg}");
        assert!(msg.contains("Verified: `npm test` exit 0") && msg.contains("Fixes #7"), "{msg}");
        let body = pr_body(&out);
        assert!(body.contains("## Reproduction") && body.contains("new test catches the bug"), "{body}");
        assert!(body.contains("## Verification") && body.contains("`npm test`: exit 0"), "{body}");
    }
}
