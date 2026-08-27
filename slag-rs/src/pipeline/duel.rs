//! duel — multi-cast forging (plan section 9, adaptive casts). Two or
//! three smiths solve the same ingot in isolated worktrees under
//! opposing directions (minimal / robust / creative). Proofs gate, the
//! assayer ranks: a cast that fails `:proof` never reaches the judge.
//! Three casts are judged pairwise round-robin. Winner merges via the
//! same branch/merge mechanics solo ingots use; losers are discarded
//! with the critique seeding the next round.

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

/// Escalation trigger (adaptive casts): a 2-cast verdict this close is a
/// coin flip, so the next round adds a third, creative cast for a real
/// spread. Round and spend caps still bind.
const ESCALATE_MARGIN: u8 = 10;

const DIRECTION_A: &str = "\nDirection: minimal — smallest correct change.";
const DIRECTION_B: &str = "\nDirection: robust — defensive, thorough.";
const DIRECTION_C: &str = "\nDirection: creative — question the obvious implementation; a \
different architecture is welcome if the proof passes.";

const CAST_LABELS: [char; 3] = ['a', 'b', 'c'];

fn direction_for(label: char) -> &'static str {
    match label {
        'a' => DIRECTION_A,
        'b' => DIRECTION_B,
        _ => DIRECTION_C,
    }
}


#[derive(Debug)]
pub enum DuelOutcome {
    /// Winner's worktree merged into the main branch; ingot is done.
    Merged { winner: char, rounds: u8 },
    /// Both casts failed proof in one round. Caller falls back to the
    /// normal single-smith strike; the duel stays off for this ingot.
    FellThrough,
}

/// Duel policy, expressed through the adaptive cast count: an ingot
/// duels when `casts_for` resolves to two or more. `:casts` pins and
/// `:duel t`/`:duel nil` overrides win over the configured mode;
/// sequential (`:solo nil`) ingots never duel — two casts of overlapping
/// sequential work is a merge-conflict factory.
pub fn should_duel(cfg: &EngineConfig, ingot: &Ingot) -> bool {
    cfg.casts_for(ingot) >= 2
}

/// Run the multi-cast duel loop for one ingot from `repo`.
///
/// Per round: `n` fresh worktrees off the current base (n = 2 or 3), all
/// casts run concurrently under opposing directions (minimal / robust /
/// creative), each proof-checks inside its own worktree. All fail → fall
/// through. One passes → merge it (margin 100). Two pass → the assayer
/// rules pairwise; three pass → pairwise round-robin (AB, AC, BC), most
/// wins takes the round. Margin >= `MARGIN_STOP` or the final round
/// merges the winner, anything less discards all and re-casts with the
/// critique — escalating a too-close 2-cast round to 3 casts.
pub async fn duel_ingot<F>(
    repo: &Path,
    ingot: &Ingot,
    cfg: &EngineConfig,
    hooks: &EngineHooks,
    casts: &F,
    judge_provider: &dyn Provider,
    initial_casts: u8,
) -> Result<DuelOutcome, SlagError>
where
    // Builds the smith for one cast: `('a' | 'b' | 'c', worktree_root)` → smith.
    F: Fn(char, &Path) -> Box<dyn Smith> + Send + Sync,
{
    let _slot = DUEL_SLOT.lock().await;
    // Each round burns one heat; never schedule more rounds than the
    // ingot's remaining heat budget (:max - :heat) allows.
    let remaining = ingot.max.saturating_sub(ingot.heat).max(1);
    let rounds = cfg.duel_rounds(ingot.grade).clamp(1, remaining);
    let mut n = initial_casts.clamp(2, 3) as usize;
    let mut critique: Option<String> = None;
    let mut prev_winner_score: Option<u8> = None;

    if !tui::is_quiet() {
        println!(
            "    {}⚔{} duel: {} cast{} ({} vs {}) — {} round{}",
            super::fg(tui::BRIGHT),
            super::reset(),
            n,
            if n == 1 { "" } else { "s" },
            cfg.model_base,
            cfg.model_alt,
            rounds,
            if rounds == 1 { "" } else { "s" },
        );
    }

    for round in 1..=rounds {
        emit(&hooks.events, EngineEvent::DuelRound { id: ingot.id.clone(), round });
        heat_tick(repo, ingot, hooks).await;

        let labels = &CAST_LABELS[..n];
        let mut ids: Vec<String> = Vec::with_capacity(n);
        let mut dirs: Vec<PathBuf> = Vec::with_capacity(n);
        for &label in labels {
            let id = format!("{}-r{round}{label}", ingot.id);
            match worktree::create_in(repo, &id).await {
                Ok(dir) => {
                    ids.push(id);
                    dirs.push(dir);
                }
                Err(e) => {
                    for made in &ids {
                        worktree::discard_in(repo, made).await;
                    }
                    return Err(e);
                }
            }
        }

        let prompts: Vec<String> = labels
            .iter()
            .map(|&l| cast_prompt(ingot, direction_for(l), critique.as_deref()))
            .collect();
        let smiths: Vec<Box<dyn Smith>> =
            labels.iter().zip(&dirs).map(|(&l, dir)| casts(l, dir)).collect();

        let run = |i: usize| run_cast(smiths[i].as_ref(), &prompts[i], &dirs[i], ingot, hooks);
        let joined: Vec<Result<Option<CastResult>, SlagError>> = if n == 3 {
            let (a, b, c) = tokio::join!(run(0), run(1), run(2));
            vec![a, b, c]
        } else {
            let (a, b) = tokio::join!(run(0), run(1));
            vec![a, b]
        };

        // Cancelled / budget-exhausted: abort the duel, don't treat as
        // failed casts.
        let mut results: Vec<Option<CastResult>> = Vec::with_capacity(n);
        let mut hard_err: Option<SlagError> = None;
        for outcome in joined {
            match outcome {
                Ok(cast) => results.push(cast),
                Err(e) => {
                    hard_err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = hard_err {
            for id in &ids {
                worktree::discard_in(repo, id).await;
            }
            return Err(e);
        }

        let passing: Vec<usize> =
            results.iter().enumerate().filter_map(|(i, c)| c.is_some().then_some(i)).collect();

        if passing.is_empty() {
            for id in &ids {
                worktree::discard_in(repo, id).await;
            }
            if !tui::is_quiet() {
                println!(
                    "    {}⚔✗{} all casts failed proof — single-smith fallback",
                    super::fg(tui::WARM),
                    super::reset()
                );
            }
            return Ok(DuelOutcome::FellThrough);
        }

        if passing.len() == 1 {
            let w = passing[0];
            let losers: Vec<String> =
                ids.iter().enumerate().filter(|&(i, _)| i != w).map(|(_, id)| id.clone()).collect();
            return crown(repo, ingot, hooks, labels[w], &ids[w], &dirs[w], &losers, round).await;
        }

        // Two or three proof-passing casts: the assayer rules. On a
        // 3-way tie (cyclic pairwise wins) `winner_score` is None so the
        // plateau stop cannot fire on a meaningless score.
        let ruled = if passing.len() == 3 {
            let (a, b, c) = (
                results[0].as_ref().expect("passing"),
                results[1].as_ref().expect("passing"),
                results[2].as_ref().expect("passing"),
            );
            judge::assay3(
                judge_provider,
                &cfg.model_judge,
                &ingot.work,
                a,
                b,
                c,
                critique.as_deref(),
            )
            .await
            .map(|v| {
                let score = (!v.tie).then_some(v.winner_score);
                (v.winner, v.margin, score, v.critique)
            })
        } else {
            let (i, j) = (passing[0], passing[1]);
            // Visual assay stays a 2-cast affair: only a full twin-cast
            // round has exactly the two dirs the config command expects.
            let images = if n == 2 {
                capture_images(cfg, ingot, &dirs[i], &dirs[j]).await
            } else {
                None
            };
            judge::assay(
                judge_provider,
                &cfg.model_judge,
                &ingot.work,
                results[i].as_ref().expect("passing"),
                results[j].as_ref().expect("passing"),
                critique.as_deref(),
                images,
            )
            .await
            .map(|v| {
                let (winner, score) = if v.winner == 'a' {
                    (labels[i], v.score_a)
                } else {
                    (labels[j], v.score_b)
                };
                (winner, v.margin(), Some(score), v.critique)
            })
        };
        let (winner, margin, winner_score, round_critique) = match ruled {
            Ok(v) => v,
            Err(e) => {
                for id in &ids {
                    worktree::discard_in(repo, id).await;
                }
                return Err(e);
            }
        };
        emit(
            &hooks.events,
            EngineEvent::DuelVerdict { id: ingot.id.clone(), winner, margin },
        );

        let plateau = winner_score.is_some_and(|score| {
            prev_winner_score.is_some_and(|prev| score.saturating_sub(prev) < PLATEAU_GAIN)
        });
        if margin >= MARGIN_STOP || plateau || round == rounds {
            let w = labels.iter().position(|&l| l == winner).expect("winner label");
            let merged = merge_winner(repo, ingot, winner, &ids[w], &dirs[w]).await;
            for (k, id) in ids.iter().enumerate() {
                if k != w {
                    worktree::discard_in(repo, id).await;
                }
            }
            if let Err(e) = merged {
                // Keep the proven winner's worktree and branch — its
                // commit is the only copy of the work. The stale
                // deterministic names are reclaimed by
                // `worktree::create_in` on the next duel attempt.
                return Err(e);
            }
            append_ledger(repo, ingot, winner, round);
            return Ok(DuelOutcome::Merged { winner, rounds: round });
        }
        // Convergence not reached: discard all, re-cast with critique. A
        // too-close 2-cast verdict escalates the next round to 3 casts —
        // the rounds clamp above still caps the total heat spend.
        for id in &ids {
            worktree::discard_in(repo, id).await;
        }
        critique = Some(round_critique);
        if let Some(score) = winner_score {
            prev_winner_score = Some(score);
        }
        if n == 2 && margin < ESCALATE_MARGIN {
            n = 3;
        }
    }

    // Unreachable: the final round always merges or falls through above.
    Ok(DuelOutcome::FellThrough)
}

/// One cast merges uncontested (its rivals failed proof): margin 100.
#[allow(clippy::too_many_arguments)]
async fn crown(
    repo: &Path,
    ingot: &Ingot,
    hooks: &EngineHooks,
    winner: char,
    win_id: &str,
    win_dir: &Path,
    lose_ids: &[String],
    round: u8,
) -> Result<DuelOutcome, SlagError> {
    let merged = merge_winner(repo, ingot, winner, win_id, win_dir).await;
    for lose_id in lose_ids {
        worktree::discard_in(repo, lose_id).await;
    }
    if let Err(e) = merged {
        // Keep the proven winner's worktree/branch (only copy of the work);
        // `worktree::create_in` reclaims the stale names on the next duel.
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

/// Run one cast to a proof-checked `CastResult`, or `Ok(None)` on any
/// cast failure (smith error, missing CMD, CMD failure, proof failure).
/// Transient provider errors (429/5xx blips) are absorbed exactly like
/// `strike_ingot`'s heats — a rate-limit burst must not burn a duel round
/// or end the duel via a phantom double-failure. `SlagError::Cancelled`
/// and run-budget exhaustion propagate so Ctrl-C / the spend cap abort
/// the duel instead of reading as a failed cast. Mirrors `strike_ingot`'s
/// CMD-then-proof sequence, rooted in the worktree.
async fn run_cast(
    smith: &dyn Smith,
    prompt: &str,
    dir: &Path,
    ingot: &Ingot,
    hooks: &EngineHooks,
) -> Result<Option<CastResult>, SlagError> {
    let invoked = super::forge::invoke_absorbing_transients(
        smith,
        prompt,
        &ingot.id,
        super::forge::MAX_TRANSIENT_PER_HEAT,
        hooks,
    )
    .await;
    let response = match invoked {
        Ok(response) => response,
        Err(e @ (SlagError::Cancelled | SlagError::RunBudgetExhausted { .. })) => return Err(e),
        Err(_) => return Ok(None),
    };
    let Some(cmd) = proof::extract_cmd(&response) else {
        return Ok(None);
    };

    let (ok, output) = run_shell_in(&cmd, dir).await;
    if !ok {
        return Ok(None);
    }

    let proof_output = if !ingot.proof.is_empty() && ingot.proof != cmd && ingot.proof != "true" {
        let (proof_ok, proof_out) = run_shell_in(&ingot.proof, dir).await;
        if !proof_ok {
            return Ok(None);
        }
        proof_out
    } else {
        output
    };

    // Stage everything so new files show in the diff (and pre-stage the
    // winner's merge commit).
    let _ = git_in(dir, &["add", "-A"]).await;
    let diff = git_in(dir, &["diff", "--cached"]).await.unwrap_or_default();

    Ok(Some(CastResult { diff, proof_output }))
}

/// How long merge_winner waits for another anvil's uncommitted overlap in
/// the main checkout to clear before letting git report the failure itself.
const MERGE_DIRTY_TRIES: u32 = 20;
const MERGE_DIRTY_WAIT_MS: u64 = 500;

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
    // `diff --cached --quiet` exits non-zero when work is staged. A
    // swallowed commit failure (hook, gpgsign, identity) would leave the
    // cast branch at base: the merge then reports "Already up to date"
    // and the winner's work silently never lands on main.
    let staged = git_in(cast_dir, &["diff", "--cached", "--quiet"]).await.is_none();
    let committed = git_in(cast_dir, &["commit", "-m", &msg, "--quiet"]).await.is_some();
    if staged && !committed {
        return Err(SlagError::WorktreeError(format!(
            "cast {winner} commit failed in {}; the work stays in that worktree",
            cast_dir.display()
        )));
    }

    let branch = format!("forge/{cast_id}");
    for _ in 0..MERGE_DIRTY_TRIES {
        {
            // Serialize against other anvils' `git add -A; git commit` on
            // the shared main checkout (see proof::REPO_GIT_LOCK).
            let _guard = crate::proof::REPO_GIT_LOCK.lock().await;
            if !dirty_overlap(repo, &branch).await {
                return worktree::merge_and_cleanup_in(repo, cast_id).await;
            }
        }
        // Another anvil's smith has uncommitted edits to files this merge
        // touches; wait for its commit instead of aborting immediately and
        // burning the proven winner.
        tokio::time::sleep(std::time::Duration::from_millis(MERGE_DIRTY_WAIT_MS)).await;
    }
    let _guard = crate::proof::REPO_GIT_LOCK.lock().await;
    worktree::merge_and_cleanup_in(repo, cast_id).await
}

/// True when the main checkout has uncommitted (or untracked) changes to
/// any file the branch's merge would touch — the case where `git merge`
/// refuses with "local changes would be overwritten".
async fn dirty_overlap(repo: &Path, branch: &str) -> bool {
    let changed = git_in(repo, &["diff", "--name-only", &format!("HEAD...{branch}")])
        .await
        .unwrap_or_default();
    if changed.trim().is_empty() {
        return false;
    }
    let status = git_in(repo, &["status", "--porcelain"]).await.unwrap_or_default();
    let dirty = porcelain_paths(&status);
    changed.lines().any(|f| dirty.contains(f))
}

/// Paths named by `git status --porcelain` output. Rename/copy lines read
/// `XY orig -> dest`; both sides count as dirty — taking the raw remainder
/// (`"orig -> dest"`) would match neither, hiding the overlap from the
/// merge gate.
fn porcelain_paths(status: &str) -> std::collections::HashSet<&str> {
    let mut paths = std::collections::HashSet::new();
    for line in status.lines() {
        let Some(rest) = line.get(3..) else { continue };
        match rest.split_once(" -> ") {
            Some((from, to)) => {
                paths.insert(from);
                paths.insert(to);
            }
            None => {
                paths.insert(rest);
            }
        }
    }
    paths
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
            bar: String::new(),
            work: "duel task".into(),
            duel,
            casts: None,
            extra: vec![],
        }
    }

    #[test]
    fn porcelain_paths_split_rename_lines_into_both_sides() {
        let status = " M src/lib.rs\nR  src/old.rs -> src/new.rs\n?? notes.txt\n";
        let paths = porcelain_paths(status);
        assert!(paths.contains("src/lib.rs"));
        assert!(paths.contains("notes.txt"));
        // Regression: a staged rename must dirty BOTH sides — the raw
        // remainder "src/old.rs -> src/new.rs" matches neither, so a
        // merge touching the renamed file sailed past the overlap gate.
        assert!(paths.contains("src/old.rs"), "{paths:?}");
        assert!(paths.contains("src/new.rs"), "{paths:?}");
        assert!(!paths.contains("src/old.rs -> src/new.rs"));
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
                    model: None,
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
            2,
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

        let outcome = duel_ingot(&repo, &ingot, &cfg(DuelMode::On), &hooks, &casts, &NoJudge, 2)
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
    async fn failed_merge_aborts_and_preserves_the_winner() {
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
            &["i5-r1b".to_string()],
            1,
        )
        .await
        .expect_err("conflicting merge must fail");
        assert!(matches!(err, SlagError::WorktreeError(_)), "got {err}");

        // No mid-merge state left in the main checkout.
        assert!(!repo.join(".git/MERGE_HEAD").exists(), "merge must be aborted");
        assert!(!std::fs::read_to_string(repo.join("x.txt")).unwrap().contains("<<<<<<<"));

        // The loser is gone; the proven winner's worktree and branch stay
        // (only copy of the work). The stale names must still be reusable:
        // create_in reclaims them on the next duel attempt.
        assert!(!repo.parent().unwrap().join("slag-anvil-i5-r1b").exists());
        assert!(repo.parent().unwrap().join("slag-anvil-i5-r1a").exists(), "winner preserved");
        let branches = git(&repo, &["branch", "--list", "forge/i5-r1a"]);
        assert!(!String::from_utf8_lossy(&branches.stdout).trim().is_empty(), "winner branch kept");
        assert!(worktree::create_in(&repo, "i5-r1a").await.is_ok(), "names must be reusable");
        worktree::discard_in(&repo, "i5-r1a").await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_commit_surfaces_instead_of_false_merge() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, repo) = test_repo();
        // Failing pre-commit hook: worktrees share the main repo's hooks.
        // Stand-in for gpgsign failures / missing identity in real repos.
        let hooks_dir = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let hook = hooks_dir.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let win_dir = worktree::create_in(&repo, "i6-r1a").await.unwrap();
        worktree::create_in(&repo, "i6-r1b").await.unwrap();
        std::fs::write(win_dir.join("won.txt"), "winner\n").unwrap();

        let ingot = ingot("i6", true, 4, None, "true");
        let err = crown(
            &repo,
            &ingot,
            &EngineHooks::default(),
            'a',
            "i6-r1a",
            &win_dir,
            &["i6-r1b".to_string()],
            1,
        )
        .await
        .expect_err("commit failure must not be reported as a merged win");
        assert!(err.to_string().contains("commit failed"), "got {err}");
        assert!(!repo.join("won.txt").exists(), "nothing must land on main");
        worktree::discard_in(&repo, "i6-r1a").await;
    }

    #[tokio::test]
    async fn dirty_overlap_detects_only_overlapping_files() {
        let (_tmp, repo) = test_repo();
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "add shared", "--quiet"]);

        let dir = worktree::create_in(&repo, "ov-r1a").await.unwrap();
        std::fs::write(dir.join("shared.txt"), "cast\n").unwrap();
        assert!(git_in(&dir, &["commit", "-am", "cast edit"]).await.is_some());

        assert!(!dirty_overlap(&repo, "forge/ov-r1a").await, "clean main: no overlap");
        std::fs::write(repo.join("unrelated.txt"), "dirt\n").unwrap();
        assert!(!dirty_overlap(&repo, "forge/ov-r1a").await, "disjoint dirt: no overlap");
        std::fs::write(repo.join("shared.txt"), "main-dirt\n").unwrap();
        assert!(dirty_overlap(&repo, "forge/ov-r1a").await, "overlapping dirt detected");
        worktree::discard_in(&repo, "ov-r1a").await;
    }

    /// Smith that reports cancellation (Ctrl-C mid-cast).
    struct CancelledSmith;

    impl Smith for CancelledSmith {
        fn invoke(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
            Box::pin(async { Err(SlagError::Cancelled) })
        }
    }

    /// Smith that pops one scripted result per invoke; shareable across
    /// the per-round cast rebuilds via the Arc wrapper below.
    struct ScriptedCast(std::sync::Mutex<std::collections::VecDeque<Result<String, SlagError>>>);

    struct SharedSmith(std::sync::Arc<ScriptedCast>);

    impl Smith for SharedSmith {
        fn invoke(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
            let next = self.0 .0.lock().unwrap().pop_front().expect("cast script exhausted");
            Box::pin(async move { next })
        }
    }

    #[tokio::test]
    async fn transient_provider_blips_do_not_fail_a_cast() {
        let (_tmp, repo) = test_repo();
        let ingot = ingot("it", true, 4, None, "test -f out.txt");

        // Cast A hits a 429 blip, then succeeds on the absorbed retry;
        // cast B genuinely fails its proof. Without transient absorption
        // the blip would read as a failed cast and (None, None) would end
        // the duel via FellThrough.
        let a = std::sync::Arc::new(ScriptedCast(std::sync::Mutex::new(
            [
                Err(SlagError::ProviderTransient("429: slow down".into())),
                Ok("CMD: echo forged-by-a > out.txt".to_string()),
            ]
            .into_iter()
            .collect(),
        )));
        let b = std::sync::Arc::new(ScriptedCast(std::sync::Mutex::new(
            [Ok("CMD: false".to_string())].into_iter().collect(),
        )));
        let casts = move |cast: char, _dir: &Path| -> Box<dyn Smith> {
            Box::new(SharedSmith(if cast == 'a' { a.clone() } else { b.clone() }))
        };

        let outcome = duel_ingot(
            &repo,
            &ingot,
            &cfg(DuelMode::On),
            &EngineHooks::default(),
            &casts,
            &NoJudge,
            2,
        )
        .await
        .expect("duel runs");

        match outcome {
            DuelOutcome::Merged { winner, rounds } => {
                assert_eq!(winner, 'a', "the blipped cast must still win");
                assert_eq!(rounds, 1);
            }
            other => panic!("transient blip must not end the duel: {other:?}"),
        }
        assert!(repo.join("out.txt").exists());
    }

    #[tokio::test]
    async fn cancelled_cast_aborts_the_duel_and_cleans_up() {
        let (_tmp, repo) = test_repo();
        let ingot = ingot("ic", true, 4, None, "true");
        let casts = |_cast: char, _dir: &Path| Box::new(CancelledSmith) as Box<dyn Smith>;

        let err = duel_ingot(
            &repo,
            &ingot,
            &cfg(DuelMode::On),
            &EngineHooks::default(),
            &casts,
            &NoJudge,
            2,
        )
        .await
        .expect_err("cancellation must abort the duel, not read as a failed cast");
        assert!(matches!(err, SlagError::Cancelled), "got {err}");
        assert!(!repo.parent().unwrap().join("slag-anvil-ic-r1a").exists());
        assert!(!repo.parent().unwrap().join("slag-anvil-ic-r1b").exists());
    }

    #[tokio::test]
    async fn rounds_clamped_to_remaining_heat_budget() {
        let (_tmp, repo) = test_repo();
        // heat 4 of max 5 → one heat left; grade 5 would otherwise cap at 10.
        let mut ing = ingot("ihb", true, 5, None, "true");
        ing.heat = 4;
        let casts = casts_returning("CMD: echo a > a.txt", "CMD: echo b > b.txt");
        // Margin 10 (< MARGIN_STOP): only the round cap can end this duel,
        // and the judge holds exactly one round's worth of replies.
        let judge = ScriptedJudge::new(&[
            r#"{"winner":"a","score_a":60,"score_b":50,"critique":"r1"}"#,
            r#"{"winner":"b","score_a":50,"score_b":60,"critique":"r1s"}"#,
        ]);

        let outcome = duel_ingot(
            &repo,
            &ing,
            &cfg(DuelMode::On),
            &EngineHooks::default(),
            &casts,
            &judge,
            2,
        )
        .await
        .expect("duel runs");
        match outcome {
            DuelOutcome::Merged { rounds, .. } => {
                assert_eq!(rounds, 1, "must merge at the clamped single round");
            }
            other => panic!("expected merge, got {other:?}"),
        }
    }

    /// Casts that record which labels ran (in scheduling order) and
    /// always produce a passing CMD unique to their label.
    fn recording_casts(
        seen: std::sync::Arc<std::sync::Mutex<Vec<char>>>,
    ) -> impl Fn(char, &Path) -> Box<dyn Smith> + Send + Sync {
        move |cast, _dir| {
            seen.lock().unwrap().push(cast);
            Box::new(MockSmith(format!("CMD: echo {cast} > out-{cast}.txt"))) as Box<dyn Smith>
        }
    }

    #[tokio::test]
    async fn close_two_cast_round_escalates_to_three_casts() {
        let (_tmp, repo) = test_repo();
        let ingot = ingot("ie", true, 4, None, "true");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let casts = recording_casts(seen.clone());
        // Round 1 (2 casts, 2 judge calls): margin 5 — below both the
        // merge stop and ESCALATE_MARGIN, so round 2 runs 3 casts.
        // Round 2 (3 casts, 6 judge calls): cast A sweeps its pairs with
        // margins 30 and 25 → min margin 25 >= MARGIN_STOP merges it.
        let judge = ScriptedJudge::new(&[
            r#"{"winner":"a","score_a":60,"score_b":55,"critique":"r1"}"#,
            r#"{"winner":"b","score_a":55,"score_b":60,"critique":"r1s"}"#,
            r#"{"winner":"a","score_a":90,"score_b":60,"critique":"ab"}"#,
            r#"{"winner":"b","score_a":60,"score_b":90,"critique":"abs"}"#,
            r#"{"winner":"a","score_a":90,"score_b":65,"critique":"ac"}"#,
            r#"{"winner":"b","score_a":65,"score_b":90,"critique":"acs"}"#,
            r#"{"winner":"a","score_a":70,"score_b":60,"critique":"bc"}"#,
            r#"{"winner":"b","score_a":60,"score_b":70,"critique":"bcs"}"#,
        ]);

        let outcome = duel_ingot(
            &repo,
            &ingot,
            &cfg(DuelMode::On),
            &EngineHooks::default(),
            &casts,
            &judge,
            2,
        )
        .await
        .expect("duel runs");

        match outcome {
            DuelOutcome::Merged { winner, rounds } => {
                assert_eq!(winner, 'a');
                assert_eq!(rounds, 2, "the escalated round must settle it");
            }
            other => panic!("expected merge, got {other:?}"),
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec!['a', 'b', 'a', 'b', 'c'],
            "round 2 must add the creative third cast"
        );
        assert_eq!(judge.calls.load(std::sync::atomic::Ordering::SeqCst), 8);
        assert!(repo.join("out-a.txt").exists(), "winner's work must land on main");
        for label in ['a', 'b', 'c'] {
            for round in [1, 2] {
                assert!(
                    !repo.parent().unwrap().join(format!("slag-anvil-ie-r{round}{label}")).exists(),
                    "worktree r{round}{label} must be cleaned up"
                );
            }
        }
    }

    #[tokio::test]
    async fn three_casts_with_single_passer_crown_without_judge() {
        let (_tmp, repo) = test_repo();
        let ingot = ingot("i3c", true, 5, None, "test -f out.txt");
        // Only the creative cast survives its proof.
        let casts = |cast: char, _dir: &Path| -> Box<dyn Smith> {
            Box::new(MockSmith(if cast == 'c' {
                "CMD: echo forged-by-c > out.txt".into()
            } else {
                "CMD: false".to_string()
            }))
        };

        let outcome = duel_ingot(
            &repo,
            &ingot,
            &cfg(DuelMode::On),
            &EngineHooks::default(),
            &casts,
            &NoJudge,
            3,
        )
        .await
        .expect("duel runs");

        match outcome {
            DuelOutcome::Merged { winner, rounds } => {
                assert_eq!(winner, 'c');
                assert_eq!(rounds, 1);
            }
            other => panic!("expected merge, got {other:?}"),
        }
        assert!(repo.join("out.txt").exists());
        for label in ['a', 'b', 'c'] {
            assert!(!repo.parent().unwrap().join(format!("slag-anvil-i3c-r1{label}")).exists());
        }
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
            2,
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
