//! duel — twin-cast forging (plan section 9). Two smiths solve the same
//! ingot in isolated worktrees under opposing directions (minimal vs
//! robust) on different model families. Proofs gate, the assayer ranks:
//! a cast that fails `:proof` never reaches the judge. Winner merges via
//! the same branch/merge mechanics solo ingots use; loser is discarded
//! with its critique seeding the next round.

use std::path::{Path, PathBuf};

use crate::anvil::worktree;
use crate::config::{EngineConfig, CRUCIBLE, LEDGER};
use crate::crucible::{Crucible, CRUCIBLE_LOCK};
use crate::engine::tools::judge::{self, CastResult};
use crate::engine::{emit, EngineEvent, Provider};
use crate::error::SlagError;
use crate::flux;
use crate::proof;
use crate::sexp::Ingot;
use crate::smith::{EngineHooks, Smith};
use crate::tui;

/// v1 bound: one duel at a time. A duel eats two anvil slots, so this
/// mutex keeps duels from stacking on top of the normal anvil fan-out.
static DUEL_SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Convergence stop: merge as soon as the assayer's margin reaches this.
const MARGIN_STOP: u8 = 20;

/// Plateau stop (plan section 9 rule 2): merge when the winner's score
/// gained less than this versus the prior round.
const PLATEAU_GAIN: u8 = 5;

const DIRECTION_A: &str = "\nDirection: minimal — smallest correct change.";
const DIRECTION_B: &str = "\nDirection: robust — defensive, thorough.";


#[derive(Debug)]
pub enum DuelOutcome {
    /// Winner's worktree merged into the main branch; ingot is done.
    Merged { winner: char, rounds: u8 },
    /// Both casts failed proof in one round. Caller falls back to the
    /// normal single-smith strike; the duel stays off for this ingot.
    FellThrough,
}

/// Duel policy: `:duel t` forces, `:duel nil` blocks, absent defers to
/// config Auto/On/Off. Sequential (`:solo nil`) ingots never duel — two
/// casts of overlapping sequential work is a merge-conflict factory.
pub fn should_duel(cfg: &EngineConfig, ingot: &Ingot) -> bool {
    if !ingot.solo {
        return false;
    }
    match ingot.duel {
        Some(force) => force,
        None => cfg.duel_qualifies(ingot.grade),
    }
}

/// Run the twin-cast duel loop for one ingot from `repo`.
///
/// Per round: two fresh worktrees off the current base, both casts run
/// concurrently, both proof-check inside their own worktree. Both fail →
/// fall through. One passes → merge it (margin 100). Both pass → the
/// assayer rules; margin >= `MARGIN_STOP` or the final round merges the
/// winner, anything less discards both and re-casts with the critique.
pub async fn duel_ingot<F>(
    repo: &Path,
    ingot: &Ingot,
    cfg: &EngineConfig,
    hooks: &EngineHooks,
    casts: &F,
    judge_provider: &dyn Provider,
) -> Result<DuelOutcome, SlagError>
where
    // Builds the smith for one cast: `('a' | 'b', worktree_root)` → smith.
    F: Fn(char, &Path) -> Box<dyn Smith> + Send + Sync,
{
    let _slot = DUEL_SLOT.lock().await;
    let rounds = cfg.duel_rounds(ingot.grade).max(1);
    let mut critique: Option<String> = None;
    let mut prev_winner_score: Option<u8> = None;

    if !tui::is_quiet() {
        println!(
            "    \x1b[38;5;220m⚔\x1b[0m duel: {} vs {} — {} round{}",
            cfg.model_base,
            cfg.model_alt,
            rounds,
            if rounds == 1 { "" } else { "s" },
        );
    }

    for round in 1..=rounds {
        emit(&hooks.events, EngineEvent::DuelRound { id: ingot.id.clone(), round });
        heat_tick(repo, ingot, hooks).await;

        let id_a = format!("{}-r{round}a", ingot.id);
        let id_b = format!("{}-r{round}b", ingot.id);
        let dir_a = worktree::create_in(repo, &id_a).await?;
        let dir_b = match worktree::create_in(repo, &id_b).await {
            Ok(dir) => dir,
            Err(e) => {
                worktree::discard_in(repo, &id_a).await;
                return Err(e);
            }
        };

        let prompt_a = cast_prompt(ingot, DIRECTION_A, critique.as_deref());
        let prompt_b = cast_prompt(ingot, DIRECTION_B, critique.as_deref());
        let smith_a = casts('a', &dir_a);
        let smith_b = casts('b', &dir_b);

        let (cast_a, cast_b) = tokio::join!(
            run_cast(smith_a.as_ref(), &prompt_a, &dir_a, ingot),
            run_cast(smith_b.as_ref(), &prompt_b, &dir_b, ingot),
        );

        match (cast_a, cast_b) {
            (None, None) => {
                worktree::discard_in(repo, &id_a).await;
                worktree::discard_in(repo, &id_b).await;
                if !tui::is_quiet() {
                    println!("    \x1b[31m⚔✗\x1b[0m both casts failed proof — single-smith fallback");
                }
                return Ok(DuelOutcome::FellThrough);
            }
            (Some(_), None) => {
                return crown(repo, ingot, hooks, 'a', &id_a, &dir_a, &id_b, round).await;
            }
            (None, Some(_)) => {
                return crown(repo, ingot, hooks, 'b', &id_b, &dir_b, &id_a, round).await;
            }
            (Some(a), Some(b)) => {
                let images = capture_images(cfg, ingot, &dir_a, &dir_b).await;
                let verdict = match judge::assay(
                    judge_provider,
                    &cfg.model_judge,
                    &ingot.work,
                    &a,
                    &b,
                    critique.as_deref(),
                    images,
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        worktree::discard_in(repo, &id_a).await;
                        worktree::discard_in(repo, &id_b).await;
                        return Err(e);
                    }
                };
                emit(
                    &hooks.events,
                    EngineEvent::DuelVerdict {
                        id: ingot.id.clone(),
                        winner: verdict.winner,
                        margin: verdict.margin(),
                    },
                );
                let winner_score = if verdict.winner == 'a' {
                    verdict.score_a
                } else {
                    verdict.score_b
                };
                let plateau = prev_winner_score
                    .is_some_and(|prev| winner_score.saturating_sub(prev) < PLATEAU_GAIN);
                if verdict.margin() >= MARGIN_STOP || plateau || round == rounds {
                    let (win_id, win_dir, lose_id) = if verdict.winner == 'a' {
                        (&id_a, &dir_a, &id_b)
                    } else {
                        (&id_b, &dir_b, &id_a)
                    };
                    let merged = merge_winner(repo, ingot, verdict.winner, win_id, win_dir).await;
                    worktree::discard_in(repo, lose_id).await;
                    if let Err(e) = merged {
                        // Merge failure must not leak the winner's
                        // worktree/branch; deterministic names would break
                        // every future duel of this ingot.
                        worktree::discard_in(repo, win_id).await;
                        return Err(e);
                    }
                    append_ledger(repo, ingot, verdict.winner, round);
                    return Ok(DuelOutcome::Merged { winner: verdict.winner, rounds: round });
                }
                // Convergence not reached: discard both, re-cast with critique.
                worktree::discard_in(repo, &id_a).await;
                worktree::discard_in(repo, &id_b).await;
                critique = Some(verdict.critique);
                prev_winner_score = Some(winner_score);
            }
        }
    }

    // Unreachable: the final round always merges or falls through above.
    Ok(DuelOutcome::FellThrough)
}

/// One cast merges uncontested (its rival failed proof): margin 100.
#[allow(clippy::too_many_arguments)]
async fn crown(
    repo: &Path,
    ingot: &Ingot,
    hooks: &EngineHooks,
    winner: char,
    win_id: &str,
    win_dir: &Path,
    lose_id: &str,
    round: u8,
) -> Result<DuelOutcome, SlagError> {
    let merged = merge_winner(repo, ingot, winner, win_id, win_dir).await;
    worktree::discard_in(repo, lose_id).await;
    if let Err(e) = merged {
        worktree::discard_in(repo, win_id).await;
        return Err(e);
    }
    emit(
        &hooks.events,
        EngineEvent::DuelVerdict { id: ingot.id.clone(), winner, margin: 100 },
    );
    append_ledger(repo, ingot, winner, round);
    Ok(DuelOutcome::Merged { winner, rounds: round })
}

/// Duel flux: the normal forge order plus the cast's direction and the
/// prior round's assayer critique.
fn cast_prompt(ingot: &Ingot, direction: &str, critique: Option<&str>) -> String {
    let mut prompt = flux::prepare_flux(ingot, None);
    prompt.push_str(direction);
    prompt.push('\n');
    if let Some(critique) = critique {
        prompt.push_str(&format!("\n[ASSAYER CRITIQUE FROM LAST ROUND]\n{critique}\n"));
    }
    prompt
}

/// Run one cast to a proof-checked `CastResult`, or None on any failure
/// (smith error, missing CMD, CMD failure, proof failure). Mirrors
/// `strike_ingot`'s CMD-then-proof sequence, rooted in the worktree.
async fn run_cast(
    smith: &dyn Smith,
    prompt: &str,
    dir: &Path,
    ingot: &Ingot,
) -> Option<CastResult> {
    let response = smith.invoke(prompt).await.ok()?;
    let cmd = proof::extract_cmd(&response)?;

    let (ok, output) = run_shell_in(&cmd, dir).await;
    if !ok {
        return None;
    }

    let proof_output = if !ingot.proof.is_empty() && ingot.proof != cmd && ingot.proof != "true" {
        let (proof_ok, proof_out) = run_shell_in(&ingot.proof, dir).await;
        if !proof_ok {
            return None;
        }
        proof_out
    } else {
        output
    };

    // Stage everything so new files show in the diff (and pre-stage the
    // winner's merge commit).
    let _ = git_in(dir, &["add", "-A"]).await;
    let diff = git_in(dir, &["diff", "--cached"]).await.unwrap_or_default();

    Some(CastResult { diff, proof_output })
}

/// Commit the winner's staged work on its cast branch, then merge and
/// clean up through the same path solo ingots use.
async fn merge_winner(
    repo: &Path,
    ingot: &Ingot,
    winner: char,
    cast_id: &str,
    cast_dir: &Path,
) -> Result<(), SlagError> {
    let msg = format!("forge({}): cast {winner} wins duel — {}", ingot.id, ingot.work);
    let _ = git_in(cast_dir, &["add", "-A"]).await;
    let _ = git_in(cast_dir, &["commit", "-m", &msg, "--quiet"]).await;
    // Serialize against other anvils' `git add -A; git commit` on the
    // shared main checkout (see proof::REPO_GIT_LOCK).
    let _guard = crate::proof::REPO_GIT_LOCK.lock().await;
    worktree::merge_and_cleanup_in(repo, cast_id).await
}

/// Visual assay inputs: only for web ingots with a configured screenshot
/// command. `{dir}` in the command expands to the worktree path; stdout's
/// last line is the image file path.
async fn capture_images(
    cfg: &EngineConfig,
    ingot: &Ingot,
    dir_a: &Path,
    dir_b: &Path,
) -> Option<(String, String)> {
    let cmd = cfg.screenshot_cmd.as_deref()?;
    if !ingot.is_web() {
        return None;
    }
    let a = screenshot(cmd, dir_a).await?;
    let b = screenshot(cmd, dir_b).await?;
    Some((a, b))
}

async fn screenshot(cmd_template: &str, dir: &Path) -> Option<String> {
    let cmd = cmd_template.replace("{dir}", &dir.to_string_lossy());
    let (ok, output) = run_shell_in(&cmd, dir).await;
    if !ok {
        return None;
    }
    let path_line = output.trim().lines().last()?.trim();
    let path = if Path::new(path_line).is_absolute() {
        PathBuf::from(path_line)
    } else {
        dir.join(path_line)
    };
    let bytes = tokio::fs::read(&path).await.ok()?;
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    };
    Some(format!("data:{mime};base64,{}", base64(&bytes)))
}

/// Minimal standard base64 (RFC 4648, with padding). No new deps.
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Same locked load-modify-save discipline as `strike_ingot`'s heat
/// update; best-effort because a duel may run where no crucible exists
/// (tests, detached repos).
async fn heat_tick(repo: &Path, ingot: &Ingot, hooks: &EngineHooks) {
    let _guard = CRUCIBLE_LOCK.lock().await;
    if let Ok(mut crucible) = Crucible::load(&repo.join(CRUCIBLE)) {
        crucible.increment_heat(&ingot.id);
        let heat = crucible.get(&ingot.id).map(|i| i.heat).unwrap_or(0);
        if crucible.save().is_ok() {
            emit(&hooks.events, EngineEvent::HeatTick { id: ingot.id.clone(), heat });
        }
    }
}

fn append_ledger(repo: &Path, ingot: &Ingot, winner: char, rounds: u8) {
    let entry = format!(
        "\n## {} [{}] gr:{} skill:{} duel\n- {}\n- cast {winner} won after {rounds} round(s)\n",
        chrono::Local::now().format("%m-%d %H:%M"),
        ingot.id,
        ingot.grade,
        ingot.skill,
        ingot.work,
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(repo.join(LEDGER))
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(entry.as_bytes())
        });
}

/// `proof::run_shell` mirrored with an explicit working directory.
async fn run_shell_in(cmd: &str, dir: &Path) -> (bool, String) {
    match tokio::process::Command::new("bash")
        .args(["-c", cmd])
        .current_dir(dir)
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            (output.status.success(), format!("{stdout}{stderr}"))
        }
        Err(e) => (false, format!("spawn error: {e}")),
    }
}

async fn git_in(dir: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DuelMode;
    use crate::engine::{ChatRequest, NormalizedResponse};
    use crate::sexp::{Skill, Status};
    use std::future::Future;
    use std::pin::Pin;

    fn cfg(duel: DuelMode) -> EngineConfig {
        EngineConfig {
            api_key: "sk-or-test".into(),
            model_base: "base/model".into(),
            model_plan: "plan/model".into(),
            model_alt: "alt/model".into(),
            model_judge: "judge/model".into(),
            effort: None,
            base_url: crate::engine::OPENROUTER_BASE.into(),
            duel,
            duel_rounds_override: None,
            screenshot_cmd: None,
        }
    }

    fn ingot(id: &str, solo: bool, grade: u8, duel: Option<bool>, proof: &str) -> Ingot {
        Ingot {
            id: id.into(),
            status: Status::Ore,
            solo,
            grade,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: proof.into(),
            work: "duel task".into(),
            duel,
            extra: vec![],
        }
    }

    #[test]
    fn duel_policy_matrix() {
        // (mode, grade, :duel field, solo) -> expected
        let cases: &[(DuelMode, u8, Option<bool>, bool, bool)] = &[
            // Auto: grade-gated at 3 (plan section 9 rule 1).
            (DuelMode::Auto, 2, None, true, false),
            (DuelMode::Auto, 3, None, true, true),
            (DuelMode::Auto, 4, None, true, true),
            (DuelMode::Auto, 5, None, true, true),
            // On/Off flip the default.
            (DuelMode::On, 1, None, true, true),
            (DuelMode::Off, 5, None, true, false),
            // :duel t forces, :duel nil blocks — regardless of mode/grade.
            (DuelMode::Off, 1, Some(true), true, true),
            (DuelMode::Auto, 1, Some(true), true, true),
            (DuelMode::On, 5, Some(false), true, false),
            (DuelMode::Auto, 5, Some(false), true, false),
            // Sequential ingots never duel, even when forced.
            (DuelMode::On, 5, Some(true), false, false),
            (DuelMode::Auto, 5, None, false, false),
        ];
        for &(mode, grade, field, solo, expected) in cases {
            let c = cfg(mode);
            let i = ingot("ix", solo, grade, field, "true");
            assert_eq!(
                should_duel(&c, &i),
                expected,
                "mode {mode:?} grade {grade} duel {field:?} solo {solo}"
            );
        }
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// Smith that always replies with a fixed script.
    struct MockSmith(String);

    impl Smith for MockSmith {
        fn invoke(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
            let text = self.0.clone();
            Box::pin(async move { Ok(text) })
        }
    }

    /// Judge that must never be reached (both-fail and one-pass paths).
    struct NoJudge;

    impl Provider for NoJudge {
        fn chat(
            &self,
            _req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            Box::pin(async { Err(SlagError::Provider("judge must not be called".into())) })
        }
    }

    /// Judge that pops canned JSON verdicts and counts calls.
    struct ScriptedJudge {
        replies: std::sync::Mutex<std::collections::VecDeque<String>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedJudge {
        fn new(replies: &[&str]) -> Self {
            Self {
                replies: std::sync::Mutex::new(replies.iter().map(|s| s.to_string()).collect()),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl Provider for ScriptedJudge {
        fn chat(
            &self,
            _req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let content = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted judge ran out of replies");
            Box::pin(async move {
                Ok(NormalizedResponse {
                    content,
                    tool_calls: vec![],
                    finish_reason: crate::engine::FinishReason::Stop,
                    reasoning: None,
                    reasoning_details: None,
                    usage: crate::engine::Usage::default(),
                })
            })
        }
    }

    /// Fresh git repo nested inside a tempdir so `../slag-anvil-*`
    /// worktrees stay contained.
    fn test_repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "forge@slag.test"],
            vec!["config", "user.name", "slag"],
            vec!["commit", "--allow-empty", "-m", "base"],
        ] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {:?}", out);
        }
        (tmp, repo)
    }

    fn casts_returning(a: &str, b: &str) -> impl Fn(char, &Path) -> Box<dyn Smith> + Send + Sync {
        let (a, b) = (a.to_string(), b.to_string());
        move |cast, _dir| {
            Box::new(MockSmith(if cast == 'a' { a.clone() } else { b.clone() })) as Box<dyn Smith>
        }
    }

    #[tokio::test]
    async fn both_failing_casts_fall_through_and_clean_up() {
        let (_tmp, repo) = test_repo();
        let ingot = ingot("i9", true, 4, None, "test -f out.txt");
        let casts = casts_returning("CMD: false", "no cmd line at all");

        let outcome = duel_ingot(
            &repo,
            &ingot,
            &cfg(DuelMode::On),
            &EngineHooks::default(),
            &casts,
            &NoJudge,
        )
        .await
        .expect("duel runs");

        assert!(matches!(outcome, DuelOutcome::FellThrough), "got {outcome:?}");
        // Worktrees and cast branches are gone.
        assert!(!repo.parent().unwrap().join("slag-anvil-i9-r1a").exists());
        assert!(!repo.parent().unwrap().join("slag-anvil-i9-r1b").exists());
        let branches = std::process::Command::new("git")
            .args(["branch", "--list", "forge/*"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
    }

    #[tokio::test]
    async fn single_passing_cast_merges_without_judge() {
        let (_tmp, repo) = test_repo();
        let ingot = ingot("i7", true, 5, None, "test -f out.txt");
        // Cast B passes its proof; cast A produces nothing usable.
        let casts = casts_returning("CMD: false", "CMD: echo forged-by-b > out.txt");

        let (tx, mut rx) = crate::engine::events::channel();
        let hooks = EngineHooks { events: Some(tx), steer: None, cancel: None };

        let outcome = duel_ingot(&repo, &ingot, &cfg(DuelMode::On), &hooks, &casts, &NoJudge)
            .await
            .expect("duel runs");

        match outcome {
            DuelOutcome::Merged { winner, rounds } => {
                assert_eq!(winner, 'b');
                assert_eq!(rounds, 1);
            }
            other => panic!("expected merge, got {other:?}"),
        }
        // Winner's work landed on main.
        assert!(repo.join("out.txt").exists());
        assert!(!repo.parent().unwrap().join("slag-anvil-i7-r1a").exists());
        assert!(!repo.parent().unwrap().join("slag-anvil-i7-r1b").exists());

        // Uncontested win reports margin 100.
        drop(hooks);
        let mut saw_verdict = false;
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::DuelVerdict { id, winner, margin } = ev {
                assert_eq!(id, "i7");
                assert_eq!(winner, 'b');
                assert_eq!(margin, 100);
                saw_verdict = true;
            }
        }
        assert!(saw_verdict, "DuelVerdict event must be emitted");
    }

    fn git(repo: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap()
    }

    #[tokio::test]
    async fn failed_merge_aborts_and_discards_both_worktrees() {
        let (_tmp, repo) = test_repo();
        // Base file both sides will touch.
        std::fs::write(repo.join("x.txt"), "base\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "add x", "--quiet"]);

        let win_dir = worktree::create_in(&repo, "i5-r1a").await.unwrap();
        worktree::create_in(&repo, "i5-r1b").await.unwrap();

        // Conflicting edits: main moves ahead, the winner cast diverges.
        std::fs::write(repo.join("x.txt"), "main-version\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "main change", "--quiet"]);
        std::fs::write(win_dir.join("x.txt"), "cast-version\n").unwrap();

        let ingot = ingot("i5", true, 4, None, "true");
        let err = crown(
            &repo,
            &ingot,
            &EngineHooks::default(),
            'a',
            "i5-r1a",
            &win_dir,
            "i5-r1b",
            1,
        )
        .await
        .expect_err("conflicting merge must fail");
        assert!(matches!(err, SlagError::WorktreeError(_)), "got {err}");

        // No mid-merge state left in the main checkout.
        assert!(!repo.join(".git/MERGE_HEAD").exists(), "merge must be aborted");
        assert!(!std::fs::read_to_string(repo.join("x.txt")).unwrap().contains("<<<<<<<"));

        // Both worktrees and forge/* branches are gone, so a redo can
        // recreate them under the same deterministic names.
        assert!(!repo.parent().unwrap().join("slag-anvil-i5-r1a").exists());
        assert!(!repo.parent().unwrap().join("slag-anvil-i5-r1b").exists());
        let branches = git(&repo, &["branch", "--list", "forge/*"]);
        assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
        assert!(worktree::create_in(&repo, "i5-r1a").await.is_ok(), "names must be reusable");
    }

    #[tokio::test]
    async fn plateau_stop_merges_before_round_cap() {
        let (_tmp, repo) = test_repo();
        // Grade 5 → 10-round cap; the plateau stop must end it at round 2.
        let ingot = ingot("i8", true, 5, None, "true");
        let casts = casts_returning("CMD: echo a > a.txt", "CMD: echo b > b.txt");
        // Round 1: margin 10 (< 20), no prior score → continue.
        // Round 2: winner gains 2 (< PLATEAU_GAIN) → merge.
        let judge = ScriptedJudge::new(&[
            r#"{"winner":"a","score_a":60,"score_b":50,"critique":"r1"}"#,
            r#"{"winner":"b","score_a":50,"score_b":60,"critique":"r1s"}"#,
            r#"{"winner":"a","score_a":62,"score_b":50,"critique":"r2"}"#,
            r#"{"winner":"b","score_a":50,"score_b":62,"critique":"r2s"}"#,
        ]);

        let outcome = duel_ingot(
            &repo,
            &ingot,
            &cfg(DuelMode::On),
            &EngineHooks::default(),
            &casts,
            &judge,
        )
        .await
        .expect("duel runs");

        match outcome {
            DuelOutcome::Merged { winner, rounds } => {
                assert_eq!(winner, 'a');
                assert_eq!(rounds, 2, "plateau must stop at round 2, not the cap");
            }
            other => panic!("expected merge, got {other:?}"),
        }
        assert_eq!(judge.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert!(repo.join("a.txt").exists(), "winner's work must land on main");
    }
}
