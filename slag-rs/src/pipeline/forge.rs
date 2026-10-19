use std::path::Path;
use std::sync::Arc;

use crate::anvil::checkpoint;
use crate::config::{DuelMode, EngineConfig, CRUCIBLE, LEDGER};
use crate::crucible::{Crucible, CRUCIBLE_LOCK};
use crate::engine::agent::ForgeAgent;
use crate::engine::events::{RunEntry, RunLog};
use crate::engine::provider::OpenRouter;
use crate::engine::tools::ToolBox;
use crate::engine::{emit, stats, transcript, Effort, EngineEvent, Provider};
use crate::error::SlagError;
use crate::flux;
use crate::proof;
use crate::sexp::{Ingot, Status};
use crate::smith::{EngineHooks, Smith};
use crate::tui;

use super::{duel, resmelt};

/// Transient provider errors (429/5xx/timeouts) absorbed per heat before
/// one surfaces and burns the heat like a real failure.
pub(crate) const MAX_TRANSIENT_PER_HEAT: u8 = 5;

struct FatalCast(Box<dyn Smith>);

enum Forged {
    Strike,
    Duel,
}

impl Smith for FatalCast {
    fn invoke(&self, prompt: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, SlagError>> + Send + '_>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            let result = self.0.invoke(&prompt).await;
            if crate::error::provider_run_failure().is_some()
                || result.as_ref().err().is_some_and(SlagError::is_deterministic_provider)
            {
                return Err(SlagError::Cancelled);
            }
            result
        })
    }
}

/// Phase 3: Forge loop — parallel anvils then sequential. The wrapper
/// anchors run-wide accounting (item 87) and brackets the run log (item
/// 81): typed metadata first, the assay verdict last.
pub async fn run(
    config: &EngineConfig,
    max_anvils: usize,
    hooks: &EngineHooks,
) -> Result<(), SlagError> {
    if crate::error::provider_run_failure().is_some() {
        return Err(SlagError::Cancelled);
    }
    stats::mark_run_start();
    let run_log = open_run_log(config);
    // Item 59: an instruction file past the cap rides every single flux.
    // Report it once at run start, not once per strike.
    for message in crate::flux::oversized_instructions() {
        emit(&hooks.events, EngineEvent::Warning { message });
    }
    let result = run_inner(config, max_anvils, hooks, &run_log).await;
    if let Ok(crucible) = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE))) {
        let counts = crucible.counts();
        run_log.append(&RunEntry::Assay {
            total: counts.total,
            forged: counts.forged,
            cracked: counts.cracked,
            ok: result.is_ok(),
        });
    }
    if crate::error::provider_run_failure().is_some() {
        return Err(SlagError::Cancelled);
    }
    result
}

/// `logs/run-<ts>-<pid>.jsonl` with the run's metadata as its first line.
fn open_run_log(config: &EngineConfig) -> RunLog {
    let now = chrono::Local::now();
    let run_id = format!("{}-{}", now.format("%Y%m%d_%H%M%S"), std::process::id());
    let meta = RunEntry::RunMeta {
        run_id: run_id.clone(),
        started: now.format("%Y-%m-%dT%H:%M:%S").to_string(),
        git_branch: git_branch(),
        model: config.model_base.clone(),
        duel: duel_label(config.duel).to_string(),
        flux_profile: crate::flux::profile(),
        crucible_hash: std::fs::read(crate::config::project_path(CRUCIBLE))
            .ok()
            .map(|b| format!("{:016x}", crate::engine::tools::fnv64(&b))),
    };
    RunLog::create(Path::new(crate::config::project_path(crate::config::LOG_DIR)), &run_id, meta)
}

fn git_branch() -> Option<String> {
    let out = crate::engine::sandbox::git()
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

fn duel_label(mode: DuelMode) -> &'static str {
    match mode {
        DuelMode::Auto => "auto",
        DuelMode::On => "on",
        DuelMode::Off => "off",
    }
}

async fn run_inner(
    config: &EngineConfig,
    max_anvils: usize,
    hooks: &EngineHooks,
    run_log: &RunLog,
) -> Result<(), SlagError> {
    let mut previous_state = None;
    loop {
        // Ctrl-C from the dashboard: stop between ingots instead of
        // marching every remaining ingot into instant-fail cracks.
        if hooks.cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst)) {
            return Err(SlagError::Cancelled);
        }

        // Locked section: load, heal stale state, pick work, mark molten.
        // At the top of the loop no anvil is running, so any Molten status
        // is stale (panicked anvil or interrupted previous run) — reset it
        // to Ore so it gets rescheduled instead of busy-spinning forever.
        let guard = CRUCIBLE_LOCK.lock().await;
        let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;

        let state: Vec<_> = crucible.ingots
            .iter()
            .map(|i| (i.id.clone(), i.status.clone(), i.heat, i.smelt))
            .collect();
        if previous_state.as_ref() == Some(&state) && crucible.has_pending() {
            return Err(SlagError::SmithFailed(
                "forge made no progress during a scheduling pass".into(),
            ));
        }
        previous_state = Some(state);

        // Item 67: a Molten ingot whose transcript is still open is a
        // crashed session, not stale state — resume the recorded
        // conversation instead of resetting to ore and burning a heat.
        let resumable = crucible
            .ingots
            .iter()
            .find(|i| {
                i.status == Status::Molten
                    && transcript::is_resumable(Path::new("."), &i.id, i.heat)
            })
            .cloned();
        if let Some(ingot) = resumable {
            drop(guard);
            // Cancel/budget propagate as Err; success and heat-failure
            // both update the crucible and the loop reschedules.
            resume_crashed_ingot(config, &ingot, hooks, run_log).await?;
            continue;
        }

        if crucible.reset_stale_molten() > 0 {
            crucible.save()?;
        }

        if !crucible.has_pending() {
            // Check for cracked. Either way the run is over: ping the
            // user who tabbed away (tui::notify gates on 6s of keyboard
            // idleness, so an attentive user never gets double-told).
            let counts = crucible.counts();
            if counts.cracked > 0 {
                tui::notify(
                    "slag",
                    &format!("forge finished — {} ingot(s) cracked", counts.cracked),
                );
                return Err(SlagError::ForgeFailed(counts.cracked));
            }
            tui::notify(
                "slag",
                &format!("forge complete — {} ingot(s) forged", counts.forged),
            );
            return Ok(());
        }

        // Run-wide spend cap: stop scheduling new ingots once the run
        // total (accumulated by every agent session) crosses the cap.
        // Pending ore stays ore — `slag resume` picks it back up.
        if let Some(cap) = crate::config::run_cost_cap() {
            let spent = crate::config::run_spend_dollars();
            if spent >= cap {
                let note = run_budget_note(spent, cap, crucible.counts().ore);
                drop(guard);
                emit(&hooks.events, EngineEvent::Warning { message: note.clone() });
                if !tui::is_quiet() {
                    println!(
                        "\n  {}⚠{} {note}",
                        super::fg(tui::BRIGHT),
                        super::reset()
                    );
                }
                append_assay_note(&note);
                run_log.append(&RunEntry::Note { message: note });
                tui::notify("slag", "run budget exhausted — forge paused");
                return Ok(());
            }
        }

        // --- Parallel anvils for :solo t ---
        let solo_ids: Vec<String> = crucible
            .solo_ore()
            .iter()
            .take(max_anvils)
            .map(|i| i.id.clone())
            .collect();

        if !solo_ids.is_empty() {
            // Mark as molten
            for id in &solo_ids {
                crucible.set_status(id, Status::Molten);
            }
            crucible.save()?;

            // Snapshot ingots before spawning (each task gets its own copy)
            let ingot_snapshots: Vec<Ingot> = solo_ids
                .iter()
                .filter_map(|id| crucible.get(id).cloned())
                .collect();
            drop(guard);

            if !tui::is_quiet() {
                println!(
                    "\n  {}⚒{}⚒{}{}⚒{} {}{} anvils:{} {}{}{}{}",
                    super::fg(tui::HOT),
                    super::fg(tui::BRIGHT),
                    super::bold(),
                    super::fg(tui::PURE),
                    super::reset(),
                    super::fg(tui::COLD),
                    solo_ids.len(),
                    super::reset(),
                    super::bold(),
                    super::fg(tui::PURE),
                    solo_ids.join(" "),
                    super::reset(),
                );
            }

            // Spawn parallel tasks
            let mut set = tokio::task::JoinSet::new();
            let batch = ingot_snapshots.clone();
            for ingot in ingot_snapshots {
                let smith = crate::smith::make_smith(config, ingot.skill.as_str(), ingot.grade, hooks);
                let task_hooks = hooks.clone();
                let n_casts = effective_casts(config, &ingot);
                let duel_cfg = (n_casts >= 2).then(|| (config.clone(), n_casts));
                // The warden needs a config on every path, not only the
                // duelling one, so the anvil carries its own clone.
                let task_config = config.clone();
                set.spawn(async move {
                    emit(
                        &task_hooks.events,
                        EngineEvent::IngotStart { id: ingot.id.clone(), work: ingot.work.clone() },
                    );
                    let duel = duel_cfg.as_ref().map(|(cfg, n)| (cfg, *n));
                    let result =
                        forge_ingot(&ingot, smith.as_ref(), duel, &task_hooks, &task_config).await;
                    (ingot.id.clone(), result)
                });
            }

            // Collect results and update crucible on main thread
            let mut cancelled = false;
            let mut budget_stop: Option<(f64, f64)> = None;
            while let Some(result) = set.join_next().await {
                if let Some(error) = crate::error::provider_run_failure() {
                    set.abort_all();
                    while set.join_next().await.is_some() {}
                    restore_unfinished_batch(&batch).await?;
                    return Err(error);
                }
                match result {
                    Ok((id, Ok(kind))) => {
                        let _guard = CRUCIBLE_LOCK.lock().await;
                        let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
                        crucible.set_status(&id, Status::Forged);
                        crucible.save()?;
                        let heat = crucible.get(&id).map(|i| i.heat).unwrap_or(0);
                        let ingot = crucible.get(&id).unwrap();
                        if matches!(kind, Forged::Strike) {
                            append_ledger(ingot, heat);
                        }
                        proof::git_commit(&id, &commit_work(ingot)).await;
                        run_log.append(&RunEntry::IngotDone { id: id.clone(), ok: true, heat });
                        fire_ingot_hook(&id, true, &hooks.events).await;
                        emit(&hooks.events, EngineEvent::IngotDone { id, ok: true });
                    }
                    Ok((id, Err(SlagError::Cancelled))) => {
                        // Not a failure: put the ingot back in the queue so
                        // `slag resume` reforges it, and stop after draining.
                        let _guard = CRUCIBLE_LOCK.lock().await;
                        let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
                        crucible.set_status(&id, Status::Ore);
                        crucible.save()?;
                        cancelled = true;
                    }
                    Ok((id, Err(SlagError::RunBudgetExhausted { spent, cap }))) => {
                        // Not a smith failure: the run cap tripped mid-
                        // flight. Back to ore for `slag resume`, and stop
                        // scheduling once this batch drains.
                        let _guard = CRUCIBLE_LOCK.lock().await;
                        let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
                        crucible.set_status(&id, Status::Ore);
                        crucible.save()?;
                        budget_stop = Some((spent, cap));
                    }
                    Ok((_, Err(e))) if e.is_deterministic_provider() => {
                        set.abort_all();
                        while set.join_next().await.is_some() {}
                        restore_unfinished_batch(&batch).await?;
                        return Err(e);
                    }
                    Ok((id, Err(e))) if unverified_review(&e) => {
                        set.abort_all();
                        while set.join_next().await.is_some() {}
                        restore_unfinished_batch(&batch).await?;
                        emit(&hooks.events, EngineEvent::Warning { message: format!("warden ingot {id}: {e}") });
                        return Err(e);
                    }
                    Ok((id, Err(_))) => {
                        let ingot = {
                            let _guard = CRUCIBLE_LOCK.lock().await;
                            Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?.get(&id).cloned()
                        };
                        if let Some(ingot) = ingot {
                            let smith = crate::smith::make_base_smith(config, hooks);
                            let result = resmelt_unlocked(&ingot, smith.as_ref()).await;
                            if let Err(e) = result.as_ref() {
                                if e.is_deterministic_provider() {
                                    set.abort_all();
                                    while set.join_next().await.is_some() {}
                                    restore_unfinished_batch(&batch).await?;
                                    return result;
                                }
                            }
                            if result.is_err() {
                                let _guard = CRUCIBLE_LOCK.lock().await;
                                let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
                                crucible.set_status(&id, Status::Cracked);
                                crucible.save()?;
                                let heat = crucible.get(&id).map(|i| i.heat).unwrap_or(0);
                                run_log.append(&RunEntry::IngotDone {
                                    id: id.clone(),
                                    ok: false,
                                    heat,
                                });
                                fire_ingot_hook(&id, false, &hooks.events).await;
                                emit(&hooks.events, EngineEvent::IngotDone { id, ok: false });
                            }
                        }
                    }
                    Err(e) => {
                        // Ingot stays Molten here; the loop-top stale-molten
                        // reset reschedules it on the next iteration.
                        if !tui::is_quiet() {
                            eprintln!(
                                "  {}✗{} anvil panicked: {e}",
                                super::fg(tui::WARM),
                                super::reset()
                            );
                        }
                    }
                }
            }

            if cancelled {
                return Err(SlagError::Cancelled);
            }

            if let Some((spent, cap)) = budget_stop {
                return finish_run_over_budget(spent, cap, hooks, run_log).await;
            }

            // Show status
            let crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            if !tui::is_quiet() {
                print!("\n  ");
                tui::ingot_status_line(&crucible.counts());
                println!();
            }
            continue;
        }

        // --- Sequential for :solo nil ---
        let ingot = match crucible.next_ore() {
            Some(i) => i.clone(),
            None => continue,
        };

        crucible.set_status(&ingot.id, Status::Molten);
        crucible.save()?;
        drop(guard);

        let smith = crate::smith::make_smith(config, ingot.skill.as_str(), ingot.grade, hooks);
        emit(
            &hooks.events,
            EngineEvent::IngotStart { id: ingot.id.clone(), work: ingot.work.clone() },
        );

        // Sequential ingots never duel (plan section 10: file-overlap risk).
        let struck = strike_ingot(&ingot, smith.as_ref(), hooks, config).await;
        if let Err(SlagError::Cancelled) = struck {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            crucible.set_status(&ingot.id, Status::Ore);
            crucible.save()?;
            return Err(SlagError::Cancelled);
        }
        if let Err(SlagError::RunBudgetExhausted { spent, cap }) = struck {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            crucible.set_status(&ingot.id, Status::Ore);
            crucible.save()?;
            drop(_guard);
            return finish_run_over_budget(spent, cap, hooks, run_log).await;
        }
        if struck.as_ref().is_err_and(|e| e.is_deterministic_provider()) {
            set_back_to_ore(&ingot.id).await?;
            return struck;
        }
        if struck.as_ref().is_err_and(unverified_review) {
            set_back_to_ore(&ingot.id).await?;
            return struck;
        }
        if struck.is_ok() {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            crucible.set_status(&ingot.id, Status::Forged);
            crucible.save()?;
            let heat = crucible.get(&ingot.id).map(|i| i.heat).unwrap_or(0);
            append_ledger(&ingot, heat);
            run_log.append(&RunEntry::IngotDone { id: ingot.id.clone(), ok: true, heat });
            fire_ingot_hook(&ingot.id, true, &hooks.events).await;
            emit(&hooks.events, EngineEvent::IngotDone { id: ingot.id.clone(), ok: true });
            proof::git_commit(&ingot.id, &commit_work(&ingot)).await;
        } else {
            let base_smith = crate::smith::make_base_smith(config, hooks);
            let result = resmelt_unlocked(&ingot, base_smith.as_ref()).await;
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            if let Err(e) = result.as_ref() {
                if e.is_deterministic_provider() {
                    crucible.set_status(&ingot.id, Status::Ore);
                    crucible.save()?;
                    return result;
                }
            }
            if result.is_ok() {
                // Re-smelted: status already updated by resmelt
                crucible.save()?;
            } else {
                crucible.set_status(&ingot.id, Status::Cracked);
                crucible.save()?;
                let heat = crucible.get(&ingot.id).map(|i| i.heat).unwrap_or(0);
                run_log.append(&RunEntry::IngotDone { id: ingot.id.clone(), ok: false, heat });
                fire_ingot_hook(&ingot.id, false, &hooks.events).await;
                emit(&hooks.events, EngineEvent::IngotDone { id: ingot.id.clone(), ok: false });
            }
        }

        let crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
        if !tui::is_quiet() {
            print!("\n  ");
            tui::ingot_status_line(&crucible.counts());
            println!();
        }
    }
}

fn unverified_review(error: &SlagError) -> bool {
    matches!(error, SlagError::SmithFailed(reason) if super::warden::is_unverified_report(reason))
}

async fn resmelt_unlocked(ingot: &Ingot, smith: &dyn Smith) -> Result<(), SlagError> {
    let mut draft = {
        let _guard = CRUCIBLE_LOCK.lock().await;
        Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?
    };
    let other_ids: std::collections::HashSet<_> = draft.ingots.iter()
        .filter(|i| i.id != ingot.id).map(|i| i.id.clone()).collect();
    resmelt::resmelt_ingot(&mut draft, ingot, smith).await?;
    let replacements = draft.ingots.into_iter()
        .filter(|i| !other_ids.contains(&i.id)).collect();
    let _guard = CRUCIBLE_LOCK.lock().await;
    let mut current = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
    current.replace(&ingot.id, replacements);
    current.save()?;
    Ok(())
}

async fn restore_unfinished_batch(batch: &[Ingot]) -> Result<(), SlagError> {
    let _guard = CRUCIBLE_LOCK.lock().await;
    let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
    for baseline in batch {
        if let Some(ingot) = crucible.get_mut(&baseline.id) {
            if ingot.status != Status::Forged {
                ingot.status = Status::Ore;
                ingot.heat = baseline.heat;
            }
        }
    }
    crucible.save()?;
    Ok(())
}

async fn unburn_heat(id: &str) -> Result<(), SlagError> {
    let _guard = CRUCIBLE_LOCK.lock().await;
    let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
    if let Some(ingot) = crucible.get_mut(id) {
        ingot.heat = ingot.heat.saturating_sub(1);
    }
    crucible.save()?;
    Ok(())
}

async fn set_back_to_ore(id: &str) -> Result<(), SlagError> {
    let _guard = CRUCIBLE_LOCK.lock().await;
    let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
    crucible.set_status(id, Status::Ore);
    crucible.save()?;
    Ok(())
}

/// Forge one solo ingot: multi-cast duel when the cast count says so,
/// otherwise (or on duel fallthrough/error) the normal single-smith
/// strike.
async fn forge_ingot(
    ingot: &Ingot,
    smith: &dyn Smith,
    duel_cfg: Option<(&EngineConfig, u8)>,
    hooks: &EngineHooks,
    config: &EngineConfig,
) -> Result<Forged, SlagError> {
    if matches!(ingot.status, Status::Cracked | Status::Forged) {
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }
    if let Some((cfg, n_casts)) = duel_cfg {
        // Casts are rebuilt every round; one shared accumulator keeps the
        // ingot cost cap cumulative across all of an ingot's duel sessions.
        let cast_spend = crate::engine::agent::SpendAccum::default();
        let casts = |cast: char, root: &Path| -> Box<dyn Smith> {
            // Cast A forges on the base model, B on the alt, and the
            // creative C on the plan model — three flavors when they are
            // pinned apart, and the direction prompts carry the diversity
            // when everything routes through openrouter/auto.
            let model = match cast {
                'a' => cfg.model_base.clone(),
                'b' => cfg.model_alt.clone(),
                _ => cfg.model_plan.clone(),
            };
            Box::new(FatalCast(Box::new(
                crate::smith::native::NativeSmith::cast(
                    cfg.clone(),
                    ingot.skill.as_str(),
                    ingot.grade,
                    root.to_path_buf(),
                    &model,
                    hooks,
                )
                .with_ingot_spend(cast_spend.clone()),
            )))
        };
        // The judge's own LLM calls count against the same caps as the
        // casts: the wrapper folds their cost into `cast_spend` and the
        // run-wide spend, and stops calling once the run cap is spent.
        let judge_provider = crate::engine::agent::SpendTracked::new(
            OpenRouter::with_base_url(cfg.api_key.clone(), cfg.base_url.clone()),
            cast_spend.clone(),
        );
        // Same observability wiring as the cast smiths: heartbeats keep a
        // judge-side rate-limit wait visible, and Ctrl-C aborts it.
        if let Some(tx) = &hooks.events {
            judge_provider.set_event_sink(tx.clone());
        }
        if let Some(cancel) = &hooks.cancel {
            judge_provider.set_cancel_flag(cancel.clone());
        }

        match duel::duel_ingot(
            Path::new("."),
            ingot,
            cfg,
            hooks,
            &casts,
            &judge_provider,
            n_casts,
        )
        .await
        {
            Ok(duel::DuelOutcome::Merged { winner, rounds }) => {
                if let Some(e) = crate::error::provider_run_failure() {
                    unburn_heat(&ingot.id).await?;
                    return Err(e);
                }
                if !tui::is_quiet() {
                    println!(
                        "    {}{}⚔█{} cast {winner} merged after {rounds} round(s)",
                        super::bold(),
                        super::fg(tui::PURE),
                        super::reset()
                    );
                }
                return Ok(Forged::Duel);
            }
            Ok(duel::DuelOutcome::FellThrough) => {
                if let Some(e) = crate::error::provider_run_failure() {
                    unburn_heat(&ingot.id).await?;
                    return Err(e);
                }
                // Both casts failed a round: duel is off for this ingot,
                // fall through to the single-smith strike below.
            }
            Err(e @ (SlagError::Cancelled | SlagError::RunBudgetExhausted { .. })) => {
                if let Some(fatal) = crate::error::provider_run_failure() {
                    unburn_heat(&ingot.id).await?;
                    return Err(fatal);
                }
                return Err(e)
            }
            Err(e) if e.is_deterministic_provider() => {
                unburn_heat(&ingot.id).await?;
                return Err(e);
            }
            Err(e) => {
                if let Some(fatal) = crate::error::provider_run_failure() {
                    unburn_heat(&ingot.id).await?;
                    return Err(fatal);
                }
                if !tui::is_quiet() {
                    println!(
                        "    {}⚔✗{} duel failed ({e}) — single-smith fallback",
                        super::fg(tui::WARM),
                        super::reset()
                    );
                }
            }
        }
        // Duel rounds already ticked crucible heat; give the fallback
        // strike only the remaining budget so total attempts never
        // exceed :max.
        let mut fallback = ingot.clone();
        {
            let _guard = CRUCIBLE_LOCK.lock().await;
            if let Ok(crucible) = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE))) {
                if let Some(current) = crucible.get(&ingot.id) {
                    fallback.heat = current.heat;
                }
            }
        }
        if remaining_budget(fallback.max, fallback.heat) == 0 {
            return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
        }
        // The fallback smith shares the duel's spend accumulator: the
        // caller-built `smith` starts a fresh $0, which would let one
        // ingot spend up to 2x SLAG_MAX_COST_INGOT (duel + fallback).
        let fallback_smith = crate::smith::make_smith_with_spend(
            cfg,
            ingot.skill.as_str(),
            ingot.grade,
            hooks,
            cast_spend.clone(),
        );
        return strike_ingot(&fallback, fallback_smith.as_ref(), hooks, cfg).await.map(|_| Forged::Strike);
    }
    strike_ingot(ingot, smith, hooks, config).await.map(|_| Forged::Strike)
}

/// Heats left after `spent` of a `max` budget.
fn remaining_budget(max: u8, spent: u8) -> u8 {
    max.saturating_sub(spent)
}

/// Casts for this ingot right now: the config resolution plus crack-retry
/// escalation — an ingot back on the anvil after burning heat gets a
/// second opinion (1 → 2), unless something pinned it to one cast
/// (explicit `:casts`, `:duel nil`, `SLAG_DUEL=off`, or sequential work).
fn effective_casts(config: &EngineConfig, ingot: &Ingot) -> u8 {
    if duel::should_duel(config, ingot) {
        return config.casts_for(ingot);
    }
    if ingot.heat > 0
        && ingot.solo
        && ingot.casts.is_none()
        && ingot.duel != Some(false)
        && config.duel != DuelMode::Off
    {
        return 2;
    }
    1
}

/// True when the smith's own stderr narrator will drive the display: no
/// dashboard hook consumes events, so `NativeSmith` spawns a narrator per
/// invoke (see `smith::native`) and any other stderr writer would corrupt
/// its live line.
fn narrator_owns_stderr(hooks: &EngineHooks) -> bool {
    hooks.events.is_none()
}

/// The run cap tripped mid-flight: emit the same note the scheduler's
/// between-batches gate produces and end the run cleanly (interrupted
/// ingots are already back to ore).
async fn finish_run_over_budget(
    spent: f64,
    cap: f64,
    hooks: &EngineHooks,
    run_log: &RunLog,
) -> Result<(), SlagError> {
    let ore_left = {
        let _guard = CRUCIBLE_LOCK.lock().await;
        Crucible::load(Path::new(crate::config::project_path(CRUCIBLE))).map(|c| c.counts().ore).unwrap_or(0)
    };
    let note = run_budget_note(spent, cap, ore_left);
    emit(&hooks.events, EngineEvent::Warning { message: note.clone() });
    if !tui::is_quiet() {
        println!("\n  {}⚠{} {note}", super::fg(tui::BRIGHT), super::reset());
    }
    append_assay_note(&note);
    run_log.append(&RunEntry::Note { message: note });
    tui::notify("slag", "run budget exhausted — forge paused");
    Ok(())
}

/// Assay-ready line for a run stopped by the spend cap.
fn run_budget_note(spent: f64, cap: f64, ore_left: usize) -> String {
    format!(
        "run budget exhausted (${spent:.2} of ${cap:.2} cap) — {ore_left} ingot(s) left as ore; \
         raise SLAG_MAX_COST_RUN and `slag resume` to continue"
    )
}

/// Ledger note the assay phase surfaces alongside the final counts.
fn append_assay_note(note: &str) {
    let entry = format!(
        "\n## {} assay note\n- {note}\n",
        chrono::Local::now().format("%m-%d %H:%M"),
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::config::project_path(LEDGER))
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(entry.as_bytes())
        });
}

/// Resume a crashed forge session (item 67): rebuild the agent directly
/// on the ingot's model, let it reload the open transcript at
/// `logs/transcripts/<id>-h<heat>.jsonl`, and finish the interrupted
/// heat — no heat burned, no context thrown away. Success seals the
/// ingot like a normal strike; failure sends it back to ore with its
/// heat budget intact and rewinds the crashed attempt's checkpoint.
/// Crash-resumes of one (ingot, heat) transcript absorbed before the
/// forge closes it and falls back to the normal heat path. Each resume
/// re-pays the recorded context, and a deterministic crash cause (OOM, a
/// proof that kills the box) would otherwise re-spend it on every
/// restart forever — the run-cost cap resets with the process.
pub(crate) const MAX_RESUME_ATTEMPTS: usize = 3;

async fn resume_crashed_ingot(
    config: &EngineConfig,
    ingot: &Ingot,
    hooks: &EngineHooks,
    run_log: &RunLog,
) -> Result<(), SlagError> {
    // Resume-attempt cap: markers persist in the transcript itself, so
    // the count survives process deaths (unlike the in-process spend
    // accumulator the run cap gates on).
    let tpath = transcript::path_for(Path::new("."), &ingot.id, ingot.heat);
    if transcript::resume_attempts(&tpath) >= MAX_RESUME_ATTEMPTS {
        return back_to_ore_after_resume(
            ingot,
            hooks,
            &format!("crashed {MAX_RESUME_ATTEMPTS} resumes in a row — transcript closed"),
        )
        .await;
    }
    transcript::TranscriptWriter::new(tpath).mark_resume();
    emit(
        &hooks.events,
        EngineEvent::IngotStart { id: ingot.id.clone(), work: ingot.work.clone() },
    );
    if !tui::is_quiet() {
        println!(
            "\n  {}↻{} {}{}[{}]{} resuming interrupted heat {}/{}",
            super::fg(tui::BRIGHT),
            super::reset(),
            super::bold(),
            super::fg(tui::PURE),
            ingot.id,
            super::reset(),
            ingot.heat,
            ingot.max,
        );
    }

    // Spawned so a panicking resume is contained like a panicking anvil
    // (the JoinSet catches those) instead of killing the whole run.
    let outcome = {
        let (config, ingot, hooks) = (config.clone(), ingot.clone(), hooks.clone());
        match tokio::spawn(async move { resume_session(&config, &ingot, &hooks).await }).await {
            Ok(result) => result,
            Err(e) => Err(SlagError::SmithFailed(format!("resume panicked: {e}"))),
        }
    };

    if let Some(e) = crate::error::provider_run_failure() {
        checkpoint::rewind_attempt(Path::new("."), &ingot.id, ingot.heat);
        set_back_to_ore(&ingot.id).await?;
        return Err(e);
    }
    match outcome {
        Ok(response) => match verify_and_seal(ingot, &response).await {
            Ok(()) => {
                let _guard = CRUCIBLE_LOCK.lock().await;
                let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
                crucible.set_status(&ingot.id, Status::Forged);
                crucible.save()?;
                append_ledger(ingot, ingot.heat);
                proof::git_commit(&ingot.id, &commit_work(ingot)).await;
                fire_ingot_hook(&ingot.id, true, &hooks.events).await;
            emit(&hooks.events, EngineEvent::IngotDone { id: ingot.id.clone(), ok: true });
                run_log.append(&RunEntry::IngotDone {
                    id: ingot.id.clone(),
                    ok: true,
                    heat: ingot.heat,
                });
                Ok(())
            }
            Err(why) => back_to_ore_after_resume(ingot, hooks, &why).await,
        },
        Err(e @ SlagError::Cancelled) => {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            crucible.set_status(&ingot.id, Status::Ore);
            crucible.save()?;
            Err(e)
        }
        Err(SlagError::RunBudgetExhausted { .. }) => {
            // Back to ore; the scheduler's loop-top budget gate ends the
            // run with the standard pause note.
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            crucible.set_status(&ingot.id, Status::Ore);
            crucible.save()?;
            Ok(())
        }
        Err(e) if e.is_deterministic_provider() => {
            set_back_to_ore(&ingot.id).await?;
            Err(e)
        }
        Err(e) => back_to_ore_after_resume(ingot, hooks, &e.to_string()).await,
    }
}

/// Failed resume: ore again (heat budget intact — the transcript closed,
/// so it will not resume twice), and the crashed attempt's files rewind
/// to their attempt-start state so the next heat starts clean.
async fn back_to_ore_after_resume(
    ingot: &Ingot,
    hooks: &EngineHooks,
    why: &str,
) -> Result<(), SlagError> {
    // Belt and braces: the agent closes the transcript on any exit it
    // reaches, but a panicked resume never got there — close it here or
    // the scheduler would resume (and panic) forever.
    transcript::TranscriptWriter::new(transcript::path_for(Path::new("."), &ingot.id, ingot.heat))
        .end(false);
    let restored = checkpoint::rewind_attempt(Path::new("."), &ingot.id, ingot.heat);
    emit(
        &hooks.events,
        EngineEvent::Warning {
            message: format!(
                "[{}] resume failed ({}) — back to ore, {restored} file(s) rewound",
                ingot.id,
                crate::engine::events::preview(why, 80)
            ),
        },
    );
    if !tui::is_quiet() {
        println!(
            "    {}↻✗{} resume failed — rescheduled ({restored} file(s) rewound)",
            super::fg(tui::WARM),
            super::reset()
        );
    }
    let _guard = CRUCIBLE_LOCK.lock().await;
    let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
    crucible.set_status(&ingot.id, Status::Ore);
    crucible.save()?;
    Ok(())
}

/// One resumed agent session. The system/task bands are rebuilt as a
/// fallback, but the open transcript supersedes them inside the agent.
async fn resume_session(
    config: &EngineConfig,
    ingot: &Ingot,
    hooks: &EngineHooks,
) -> Result<String, SlagError> {
    let model = config.model_for_grade(ingot.grade).to_string();
    let provider = Arc::new(OpenRouter::with_base_url(
        config.api_key.clone(),
        config.base_url.clone(),
    ));
    if let Some(tx) = &hooks.events {
        provider.set_event_sink(tx.clone());
    }
    if let Some(cancel) = &hooks.cancel {
        provider.set_cancel_flag(cancel.clone());
    }
    let window = provider.context_length(&model).await;
    let toolbox = ToolBox::new(".")
        .with_policy(crate::engine::policy::Policy::from_config())
        .with_steer(hooks.steer.clone());
    let mut agent = ForgeAgent::new(provider, toolbox, &model)
        .with_context_window(window)
        .with_coordinator_root(std::path::PathBuf::from("."))
        .with_resume(true)
        .with_required_verification()
        .with_effort(config.effort.or(Some(Effort::from_grade(ingot.grade))));
    if let Some(tx) = &hooks.events {
        agent = agent.with_events(tx.clone());
    }
    if let Some(steer) = &hooks.steer {
        agent = agent.with_steer(steer.clone());
    }
    if let Some(cancel) = &hooks.cancel {
        agent = agent.with_cancel(cancel.clone());
    }

    let bands = crate::engine::prompt::build(
        Path::new("."),
        &model,
        crate::engine::prompt::PromptMode::Forge,
    );
    let flux_text = flux::prepare_flux(ingot, None);
    transcript::scope(
        ingot.id.clone(),
        ingot.heat,
        agent.run(bands.join(), flux_text),
    )
    .await
}

/// The strike's acceptance gate, factored for the resume path: extract
/// CMD, run it, verify the proof, commit. `Err` carries the slag line a
/// retry heat would have been fed.
async fn verify_and_seal(ingot: &Ingot, response: &str) -> Result<(), String> {
    let cmd = proof::extract_cmd(response).ok_or("NO CMD: line in response".to_string())?;
    let (ok, output) = run_verification(&cmd).await;
    if !ok {
        return Err(format!("CMD failed (exit 1): {output}"));
    }
    if !ingot.proof.is_empty() && ingot.proof != cmd && ingot.proof != "true" {
        let (proof_ok, proof_output) = run_verification(&ingot.proof).await;
        if !proof_ok {
            return Err(format!("Proof failed [{}]: {proof_output}", ingot.proof));
        }
    }
    if let Some(e) = crate::error::provider_run_failure() {
        return Err(e.to_string());
    }
    Ok(())
}

async fn run_verification(cmd: &str) -> (bool, String) {
    let check = check_verification(cmd).await;
    (check.success, check.output)
}

/// Run a CMD line or proof with the coordinator files protected, and keep
/// the shell's own verdict on whether the command could run at all.
async fn check_verification(cmd: &str) -> proof::ShellCheck {
    let refused = |output: String| proof::ShellCheck {
        success: false,
        output,
        broken: false,
        reason: String::new(),
    };
    if let Some(error) = crate::engine::agent::coordinator_command_error(cmd) {
        return refused(error.to_string());
    }
    let _guard = CRUCIBLE_LOCK.lock().await;
    let snapshot = match super::CoordinatorGuard::capture(Path::new(".")) {
        Ok(snapshot) => snapshot,
        Err(e) => return refused(format!("cannot protect coordinator files: {e}")),
    };
    if let Err(e) = super::hygiene::install_ignores(Path::new("."), None) {
        return refused(format!("cannot install cache ignores: {e}"));
    }
    let files = match super::hygiene::VerificationFiles::capture(Path::new(".")) {
        Ok(files) => files,
        Err(e) => return refused(format!("cannot snapshot verification files: {e}")),
    };
    let check = if direct_contract() { check_read_only(cmd).await } else { proof::check_shell(cmd).await };
    let restored = snapshot.restore();
    let cleaned = files.clean_new(Path::new("."));
    if let Err(e) = cleaned {
        return refused(format!("cannot remove verification files: {e}\n{}", check.output));
    }
    match restored {
        Ok(changed) if changed.is_empty() => check,
        Ok(changed) => refused(format!(
            "verification changed coordinator files: {}\n{}",
            changed.join(", "),
            check.output
        )),
        Err(e) => refused(format!("cannot restore coordinator files: {e}\n{}", check.output)),
    }
}

fn direct_contract() -> bool {
    std::fs::read_to_string(crate::config::project_path(super::warden::BAR_FILE)).ok()
        .is_some_and(|bar| bar.lines().any(|line| line.starts_with("REQUIRED RUN COMMANDS: ")))
}

async fn check_read_only(cmd: &str) -> proof::ShellCheck {
    let failed = |reason: String| proof::ShellCheck {
        success: false, output: reason.clone(), broken: false, reason,
    };
    let sandbox = match crate::engine::sandbox::Sandbox::read_only() {
        Ok(sandbox) => sandbox,
        Err(e) => return failed(format!("cannot run read-only verification: {e}")),
    };
    let Some(temp) = sandbox.temp_dir() else {
        return failed("read-only verification has no private temporary directory".to_string());
    };
    let command = match crate::engine::probe::materialize(cmd, temp) {
        Ok(command) => command,
        Err(error) => return failed(format!("cannot prepare verification probe: {error}")),
    };
    let output = match sandbox.wrap("/bin/bash").args(["-c", &command]).current_dir(".")
        .env("PYTHONDONTWRITEBYTECODE", "1").env("NODE_DISABLE_COMPILE_CACHE", "1")
        .output().await
    {
        Ok(output) => output,
        Err(e) => return failed(format!("read-only verification spawn error: {e}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let broken = matches!(output.status.code(), Some(126 | 127))
        || output.status.code() == Some(2) && stderr.lines().any(|line| {
            (line.starts_with("bash:") || line.contains(": line ") || line.contains(": -c:"))
                && (line.contains("syntax error") || line.contains("unexpected EOF")
                    || line.contains("unexpected end of file"))
        });
    let reason = stderr.lines().find(|line| !line.trim().is_empty())
        .or_else(|| stdout.lines().find(|line| !line.trim().is_empty()))
        .map(str::trim).map(str::to_string)
        .unwrap_or_else(|| format!("read-only verification exited with {:?}", output.status.code()));
    proof::ShellCheck {
        success: output.status.success(), output: format!("{stdout}{stderr}"), broken, reason,
    }
}

/// Invoke the smith, absorbing transient provider errors: up to
/// `max_transient` immediate retries that burn no heat and run no proof.
/// The retry past the limit surfaces the error, and the heat loop then
/// treats it like any smith failure — that attempt counts as a real heat.
pub(crate) async fn invoke_absorbing_transients(
    smith: &dyn Smith,
    flux: &str,
    id: &str,
    max_transient: u8,
    hooks: &EngineHooks,
) -> Result<String, SlagError> {
    let mut transients = 0u8;
    loop {
        match smith.invoke(flux).await {
            Err(SlagError::ProviderTransient(why)) if transients < max_transient => {
                transients += 1;
                emit(
                    &hooks.events,
                    EngineEvent::Warning {
                        message: format!(
                            "[{id}] transient provider error ({transients}/{max_transient}, \
                             heat not burned): {why}"
                        ),
                    },
                );
            }
            other => return other,
        }
    }
}

/// Strike a single ingot: retry with heat, extract CMD, verify proof.
async fn strike_ingot(
    ingot: &Ingot,
    smith: &dyn Smith,
    hooks: &EngineHooks,
    config: &EngineConfig,
) -> Result<(), SlagError> {
    let mut slag: Option<String> = None;
    let quiet = tui::is_quiet();

    if !quiet {
        println!(
            "\n  {}▣{} {}{}[{}]{} {}{}{}",
            super::fg(tui::HOT),
            super::reset(),
            super::bold(),
            super::fg(tui::PURE),
            ingot.id,
            super::reset(),
            tui::truncate(&ingot.work, 42),
            if ingot.is_complex() {
                format!(" {}◉{}", super::fg(tui::BRIGHT), super::reset())
            } else {
                String::new()
            },
            if ingot.is_web() {
                format!(" {}⚡{}", super::fg(tui::HOT), super::reset())
            } else {
                String::new()
            },
        );
        println!(
            "    {}gr:{} skill:{} proof:{}{}",
            super::fg(tui::COLD),
            ingot.grade,
            ingot.skill,
            tui::truncate(&ingot.proof, 30),
            super::reset(),
        );
    }

    for heat in 1..=remaining_budget(ingot.max, ingot.heat) {
        // Update heat in crucible file
        let current = {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(crate::config::project_path(CRUCIBLE)))?;
            crucible.increment_heat(&ingot.id);
            crucible.save()?;
            let current = crucible.get(&ingot.id).map(|i| i.heat).unwrap_or(heat);
            emit(&hooks.events, EngineEvent::HeatTick { id: ingot.id.clone(), heat: current });
            current
        };
        // Item 68: attempt-start snapshot boundary. The toolbox backs up
        // each file lazily before its first modification; a failed heat
        // rewinds to this boundary so the retry starts clean.
        let ckpt = checkpoint::Checkpoint::for_attempt(Path::new("."), &ingot.id, current);
        ckpt.begin();

        if !quiet {
            let hc = match current {
                1..=2 => tui::WARM,
                3 => tui::HOT,
                4 => tui::BRIGHT,
                _ => tui::PURE,
            };
            let hb = if current > 4 { super::bold() } else { String::new() };
            print!(
                "    {}{}⚒ {current}/{}{} ",
                hb,
                super::fg(hc),
                ingot.max,
                super::reset()
            );
        }

        let mut active = ingot.clone();
        active.heat = current;
        let flux_text = flux::prepare_flux(&active, slag.as_deref());
        log_to_file(&format!("FLUX_{}_{current}", ingot.id), &flux_text);

        let spinner_msg = if ingot.is_complex() {
            "planning..."
        } else if ingot.is_web() {
            "web forging..."
        } else {
            "forging..."
        };
        // In stream mode the smith's stderr narrator owns the terminal
        // line; a steady-tick indicatif spinner on the same stderr would
        // fight its `\r` rewrites, so it stays hidden there.
        let spinner = if narrator_owns_stderr(hooks) {
            indicatif::ProgressBar::hidden()
        } else {
            tui::spinner(spinner_msg)
        };

        // Item 67: the ingot scope makes the agent journal this session
        // to logs/transcripts/<id>-h<current>.jsonl for crash resume.
        let response = match transcript::scope(
            ingot.id.clone(),
            current,
            invoke_absorbing_transients(smith, &flux_text, &ingot.id, MAX_TRANSIENT_PER_HEAT, hooks),
        )
        .await
        {
            Ok(r) => {
                spinner.finish_and_clear();
                r
            }
            Err(e) => {
                spinner.finish_and_clear();
                // Ctrl-C cancellation is not a retryable smith failure:
                // retrying burns the whole heat budget on instant errors
                // and cracks the ingot. Propagate so the run stops.
                if matches!(
                    e,
                    SlagError::Cancelled | SlagError::RunBudgetExhausted { .. }
                ) {
                    return Err(e);
                }
                if e.is_deterministic_provider() {
                    ckpt.rewind();
                    unburn_heat(&ingot.id).await?;
                    return Err(e);
                }
                slag = Some(format!("Smith error: {e}"));
                if !quiet {
                    println!("{}✗{}", super::fg(tui::WARM), super::reset());
                }
                rewind_failed_heat(&ckpt, ingot, current, hooks);
                continue;
            }
        };

        if let Some(e) = crate::error::provider_run_failure() {
            ckpt.rewind();
            unburn_heat(&ingot.id).await?;
            return Err(e);
        }
        log_to_file(&format!("STRIKE_{}_{current}", ingot.id), &response);

        // Extract CMD
        let cmd = match proof::extract_cmd(&response) {
            Some(c) => c,
            None => {
                slag = Some("NO CMD: line in response".into());
                if !quiet {
                    println!("{}✗{} no CMD", super::fg(tui::WARM), super::reset());
                }
                rewind_failed_heat(&ckpt, ingot, current, hooks);
                continue;
            }
        };

        if !quiet {
            print!("{}{}{} ", super::fg(tui::COLD), tui::truncate(&cmd, 32), super::reset());
            tui::flush();
        }

        // Run CMD
        let check = check_verification(&cmd).await;
        let (ok, output) = (check.success, &check.output);
        log_to_file(
            &format!("ASSAY_{}_{current}", ingot.id),
            &format!("command: {cmd}\nexit={}\n{output}", if ok { 0 } else { 1 }),
        );
        if !ok && cmd == ingot.proof.trim() && check.broken {
            return reject_broken_proof(ingot, current, &check, hooks).await;
        }

        if ok {
            // Verify proof if different from cmd
            if !ingot.proof.is_empty() && ingot.proof != cmd && ingot.proof != "true" {
                let check = check_verification(&ingot.proof).await;
                log_to_file(&format!("ASSAY_{}_{current}_proof", ingot.id),
                    &format!("command: {}\nexit={}\n{}", ingot.proof, if check.success { 0 } else { 1 }, check.output));
                if !check.success {
                    if check.broken {
                        return reject_broken_proof(ingot, current, &check, hooks).await;
                    }
                    log_to_file(&format!("PROOF_{}_{current}", ingot.id),
                        &format!("exit=1\n{}\n{}", check.reason, check.output));
                    slag = Some(format!("Proof failed [{}]: {}", ingot.proof, check.output));
                    if !quiet {
                        println!("{}✗{} impure", super::fg(tui::WARM), super::reset());
                    }
                    rewind_failed_heat(&ckpt, ingot, current, hooks);
                    continue;
                }
            }

            log_to_file(&format!("PROOF_{}_{current}", ingot.id), "exit=0\n");

            // The sub-goal gate. `proof` has only shown that something
            // appeared; the bar asks whether this ingot's goal was met, and
            // a loss returns like any other failure -- feeding the gap to
            // the next heat, and cracking into re-smelt/SPLIT when the
            // heats run out. That is the recursion: a sub-goal too big to
            // meet becomes sub-ingots judged the same way.
            if crate::pipeline::warden::armed() && !ingot.bar.is_empty() {
                let verdict =
                    crate::pipeline::warden::judge_ingot(config, hooks, ingot).await;
                if let Some(e) = crate::error::provider_run_failure() {
                    ckpt.rewind();
                    unburn_heat(&ingot.id).await?;
                    return Err(e);
                }
                if verdict.is_unverified() {
                    unburn_heat(&ingot.id).await?;
                    return Err(SlagError::SmithFailed(verdict.gap));
                }
                if !verdict.fulfilled {
                    slag = Some(format!("Bar not met [{}]: {}", ingot.id, verdict.gap));
                    if !quiet {
                        println!("{}✗{} short of the bar", super::fg(tui::WARM), super::reset());
                    }
                    rewind_failed_heat(&ckpt, ingot, current, hooks);
                    continue;
                }
            }

            if let Some(e) = crate::error::provider_run_failure() {
                ckpt.rewind();
                unburn_heat(&ingot.id).await?;
                return Err(e);
            }
            if !quiet {
                println!("{}{}█{}", super::bold(), super::fg(tui::PURE), super::reset());
            }
            return Ok(());
        } else {
            slag = Some(format!("CMD failed (exit 1): {output}"));
            if !quiet {
                println!("{}✗{}", super::fg(tui::WARM), super::reset());
            }
            rewind_failed_heat(&ckpt, ingot, current, hooks);
        }
    }

    Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max))
}

async fn reject_broken_proof(
    ingot: &Ingot,
    heat: u8,
    check: &proof::ShellCheck,
    hooks: &EngineHooks,
) -> Result<(), SlagError> {
    let reason = format!("broken proof [{}]: {}",
        crate::engine::events::preview(&ingot.proof, 80),
        crate::engine::events::preview(&check.reason, 160));
    log_to_file(&format!("PROOF_{}_{heat}", ingot.id),
        &format!("exit=1\n{}\n{}", check.reason, check.output));
    emit(&hooks.events, EngineEvent::Warning { message: format!("[{}] {reason}", ingot.id) });
    if !tui::is_quiet() {
        println!("{}✗{} {reason}", super::fg(tui::WARM), super::reset());
    }
    unburn_heat(&ingot.id).await?;
    Err(SlagError::ProofFailed { id: ingot.id.clone(), reason })
}

/// Item 68: a failed heat with retries left rewinds the workspace to the
/// attempt-start snapshot — the retry builds from clean state, not the
/// wreck of the failed attempt. The final heat never rewinds: a cracked
/// ingot's debris is what resmelt and the operator inspect.
/// Keep direct task edits so the next heat can repair the failed proof.
fn rewind_failed_heat(
    ckpt: &checkpoint::Checkpoint,
    ingot: &Ingot,
    heat: u8,
    hooks: &EngineHooks,
) {
    if heat >= ingot.max || ingot.grade == 1 && direct_contract() {
        return;
    }
    let restored = ckpt.rewind();
    if restored > 0 {
        emit(
            &hooks.events,
            EngineEvent::Warning {
                message: format!(
                    "[{}] rewound {restored} file(s) to attempt start for a clean retry",
                    ingot.id
                ),
            },
        );
    }
}

fn append_ledger(ingot: &Ingot, heat: u8) {
    let entry = format!(
        "\n## {} [{}] gr:{} skill:{}\n- {}\n- heats:{}\n- proof: {}\n",
        chrono::Local::now().format("%m-%d %H:%M"),
        ingot.id,
        ingot.grade,
        ingot.skill,
        ingot.work,
        heat,
        ingot.proof,
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::config::project_path(LEDGER))
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(entry.as_bytes())
        });
}

fn commit_work(ingot: &Ingot) -> String {
    let ore = std::fs::read_to_string(crate::config::project_path(crate::config::ORE_FILE)).unwrap_or_default();
    commission_subject(&ore).unwrap_or_else(|| {
        if ingot.heat > 1 || ingot.smelt > 0 {
            "complete project requirements".to_string()
        } else {
            ingot.work.lines().find(|line| !line.trim().is_empty()).unwrap_or("complete project requirements").trim().chars().take(160).collect()
        }
    })
}

fn commission_subject(ore: &str) -> Option<String> {
    ore.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.chars().take(160).collect())
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::project_path(crate::config::LOG_DIR));
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn commit_subject_uses_commission_before_retry_notes() {
        assert_eq!(commission_subject("# Commission\n\nBuild a log viewer.\n\n## Repair\nInternal fix notes"), Some("Build a log viewer.".to_string()));
        assert_eq!(commission_subject("# Commission\n\n"), None);
        assert_eq!(commission_subject(&"x".repeat(200)).unwrap().len(), 160);
    }

    fn forge_test_config(duel: DuelMode) -> EngineConfig {
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

    fn forge_test_ingot(grade: u8, heat: u8) -> Ingot {
        Ingot {
            id: "ix".into(),
            status: crate::sexp::Status::Ore,
            solo: true,
            grade,
            skill: crate::sexp::Skill::Default,
            heat,
            max: 5,
            smelt: 0,
            proof: "cargo test".into(),
            bar: String::new(),
            work: "refactor the scheduler".into(),
            duel: None,
            casts: None,
            extra: vec![],
        }
    }

    #[test]
    fn crack_retry_escalates_one_cast_to_two() {
        let config = forge_test_config(DuelMode::Auto);
        // Fresh grade-1 ingot: one cast. Same ingot back with heat: two.
        let fresh = forge_test_ingot(1, 0);
        assert_eq!(effective_casts(&config, &fresh), 1);
        let retried = forge_test_ingot(1, 2);
        assert_eq!(effective_casts(&config, &retried), 2, "heat > 0 bumps 1 → 2");

        // Multi-cast resolutions pass through untouched.
        let design = forge_test_ingot(4, 1);
        assert_eq!(effective_casts(&config, &design), 2);
        let studio = forge_test_ingot(5, 0);
        assert_eq!(effective_casts(&config, &studio), 3);
    }

    #[test]
    fn crack_retry_escalation_respects_single_cast_pins() {
        let mut retried = forge_test_ingot(1, 2);

        // SLAG_DUEL=off is a kill switch, retry or not.
        assert_eq!(effective_casts(&forge_test_config(DuelMode::Off), &retried), 1);

        let config = forge_test_config(DuelMode::Auto);
        // An explicit :casts 1 pin holds through retries.
        retried.casts = Some(1);
        assert_eq!(effective_casts(&config, &retried), 1);
        retried.casts = None;
        // :duel nil blocks the escalation too.
        retried.duel = Some(false);
        assert_eq!(effective_casts(&config, &retried), 1);
        retried.duel = None;
        // Sequential work never fans out.
        retried.solo = false;
        assert_eq!(effective_casts(&config, &retried), 1);
    }

    #[test]
    fn remaining_budget_never_exceeds_max_or_underflows() {
        assert_eq!(remaining_budget(5, 0), 5);
        assert_eq!(remaining_budget(5, 2), 3);
        assert_eq!(remaining_budget(5, 5), 0);
        // Duel rounds may overshoot the budget; no wrap-around.
        assert_eq!(remaining_budget(5, 7), 0);
    }

    /// Scripted smith: pops one Result per invoke, counts invocations.
    struct ScriptSmith {
        script: Mutex<VecDeque<Result<String, SlagError>>>,
        invokes: AtomicUsize,
    }

    impl ScriptSmith {
        fn new(script: Vec<Result<String, SlagError>>) -> Self {
            Self {
                script: Mutex::new(script.into()),
                invokes: AtomicUsize::new(0),
            }
        }
    }

    impl Smith for ScriptSmith {
        fn invoke(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = Result<String, SlagError>> + Send + '_>> {
            self.invokes.fetch_add(1, Ordering::SeqCst);
            let next = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("script exhausted");
            Box::pin(async move { next })
        }
    }

    fn transient() -> Result<String, SlagError> {
        Err(SlagError::ProviderTransient("503: overloaded".into()))
    }

    #[tokio::test]
    async fn deterministic_cast_failure_aborts_the_duel() {
        let cast = FatalCast(Box::new(ScriptSmith::new(vec![Err(
            SlagError::ProviderApi(crate::error::ProviderApiError {
                category: crate::error::ProviderErrorCategory::Auth,
                status: Some(401),
                retryable: false,
                excerpt: "bad key".into(),
            }),
        )])));
        assert!(matches!(cast.invoke("flux").await, Err(SlagError::Cancelled)));
    }

    #[tokio::test]
    async fn transients_are_absorbed_without_surfacing() {
        let smith = ScriptSmith::new(vec![
            transient(),
            transient(),
            Ok("CMD: true".into()),
        ]);
        let hooks = EngineHooks::default();

        let out = invoke_absorbing_transients(&smith, "flux", "i1", 5, &hooks)
            .await
            .expect("transients absorbed");
        assert_eq!(out, "CMD: true");
        assert_eq!(smith.invokes.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn transient_past_the_limit_surfaces_and_counts_as_a_heat() {
        // 5 absorbed retries, the 6th surfaces to the heat loop.
        let smith = ScriptSmith::new((0..6).map(|_| transient()).collect());
        let hooks = EngineHooks::default();

        let err = invoke_absorbing_transients(&smith, "flux", "i1", 5, &hooks)
            .await
            .expect_err("sixth transient must surface");
        assert!(matches!(err, SlagError::ProviderTransient(_)), "got: {err}");
        assert_eq!(smith.invokes.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn permanent_errors_surface_immediately() {
        for e in [
            SlagError::SmithFailed("bad output".into()),
            SlagError::Cancelled,
            SlagError::Provider("401: bad key".into()),
        ] {
            let smith = ScriptSmith::new(vec![Err(e)]);
            let hooks = EngineHooks::default();
            invoke_absorbing_transients(&smith, "flux", "i1", 5, &hooks)
                .await
                .expect_err("permanent error surfaces");
            assert_eq!(smith.invokes.load(Ordering::SeqCst), 1, "no retry burned");
        }
    }

    #[tokio::test]
    async fn transient_retries_emit_warnings_with_the_ingot_id() {
        let smith = ScriptSmith::new(vec![transient(), Ok("CMD: true".into())]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let hooks = EngineHooks { events: Some(tx), ..Default::default() };

        invoke_absorbing_transients(&smith, "flux", "i7", 5, &hooks)
            .await
            .expect("absorbed");

        let mut warned = false;
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::Warning { message } = ev {
                assert!(message.contains("[i7]"), "message: {message}");
                assert!(message.contains("heat not burned"), "message: {message}");
                warned = true;
            }
        }
        assert!(warned, "transient retry must warn");
    }

    #[tokio::test]
    async fn run_budget_exhaustion_surfaces_without_burning_retries() {
        // The error must propagate out of the transient absorber (it is
        // not transient) so the heat loop can stop instead of retrying.
        let smith = ScriptSmith::new(vec![Err(SlagError::RunBudgetExhausted {
            spent: 5.02,
            cap: 5.0,
        })]);
        let hooks = EngineHooks::default();
        let err = invoke_absorbing_transients(&smith, "flux", "i1", 5, &hooks)
            .await
            .expect_err("budget exhaustion surfaces");
        assert!(matches!(err, SlagError::RunBudgetExhausted { .. }), "got: {err}");
        assert_eq!(smith.invokes.load(Ordering::SeqCst), 1, "no retry burned");
    }

    #[test]
    fn stream_mode_hides_the_indicatif_spinner_for_the_narrator() {
        // No dashboard hook → the per-invoke stderr narrator drives the
        // display, so strike_ingot must not start a competing spinner.
        assert!(narrator_owns_stderr(&EngineHooks::default()));
        let (tx, _rx) = crate::engine::events::channel();
        let hooks = EngineHooks { events: Some(tx), ..Default::default() };
        assert!(!narrator_owns_stderr(&hooks));
    }

    #[test]
    fn run_budget_note_reads_like_an_assay_line() {
        let note = run_budget_note(5.021, 5.0, 3);
        assert!(note.contains("$5.02 of $5.00"), "note: {note}");
        assert!(note.contains("3 ingot(s) left as ore"), "note: {note}");
        assert!(note.contains("slag resume"), "note: {note}");
    }
}

/// Item 69: the ingot lifecycle hooks. `hooks` is already a local name in
/// this module (the run's event handles), so the engine module is spelled
/// out. Fire-and-report only: an ingot's verdict is settled before this
/// runs, and no hook may overturn it.
async fn fire_ingot_hook(id: &str, ok: bool, events: &Option<crate::engine::EventTx>) {
    use crate::engine::hooks::{fire, HookEvent, HookPayload};
    let event = if ok { HookEvent::IngotForged } else { HookEvent::IngotCracked };
    fire(event, HookPayload::new(event).with_ingot(id), "", "", None, events.as_ref()).await;
}
