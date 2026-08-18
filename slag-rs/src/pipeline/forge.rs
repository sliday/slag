use std::path::Path;

use crate::config::{DuelMode, EngineConfig, CRUCIBLE, LEDGER};
use crate::crucible::{Crucible, CRUCIBLE_LOCK};
use crate::engine::provider::OpenRouter;
use crate::engine::{emit, EngineEvent, Provider};
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

/// Phase 3: Forge loop — parallel anvils then sequential
pub async fn run(
    config: &EngineConfig,
    max_anvils: usize,
    hooks: &EngineHooks,
) -> Result<(), SlagError> {
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
        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;

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
            for ingot in ingot_snapshots {
                let smith = crate::smith::make_smith(config, ingot.skill.as_str(), ingot.grade, hooks);
                let task_hooks = hooks.clone();
                let n_casts = effective_casts(config, &ingot);
                let duel_cfg = (n_casts >= 2).then(|| (config.clone(), n_casts));
                set.spawn(async move {
                    emit(
                        &task_hooks.events,
                        EngineEvent::IngotStart { id: ingot.id.clone(), work: ingot.work.clone() },
                    );
                    let duel = duel_cfg.as_ref().map(|(cfg, n)| (cfg, *n));
                    let result = forge_ingot(&ingot, smith.as_ref(), duel, &task_hooks).await;
                    (ingot.id.clone(), result)
                });
            }

            // Collect results and update crucible on main thread
            let mut cancelled = false;
            let mut budget_stop: Option<(f64, f64)> = None;
            while let Some(result) = set.join_next().await {
                match result {
                    Ok((id, Ok(()))) => {
                        let _guard = CRUCIBLE_LOCK.lock().await;
                        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                        crucible.set_status(&id, Status::Forged);
                        crucible.save()?;
                        emit(&hooks.events, EngineEvent::IngotDone { id, ok: true });
                    }
                    Ok((id, Err(SlagError::Cancelled))) => {
                        // Not a failure: put the ingot back in the queue so
                        // `slag resume` reforges it, and stop after draining.
                        let _guard = CRUCIBLE_LOCK.lock().await;
                        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                        crucible.set_status(&id, Status::Ore);
                        crucible.save()?;
                        cancelled = true;
                    }
                    Ok((id, Err(SlagError::RunBudgetExhausted { spent, cap }))) => {
                        // Not a smith failure: the run cap tripped mid-
                        // flight. Back to ore for `slag resume`, and stop
                        // scheduling once this batch drains.
                        let _guard = CRUCIBLE_LOCK.lock().await;
                        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                        crucible.set_status(&id, Status::Ore);
                        crucible.save()?;
                        budget_stop = Some((spent, cap));
                    }
                    Ok((id, Err(_))) => {
                        // Try resmelt. Holding the lock across the smith call
                        // stalls other anvils' heat ticks, but keeps the
                        // load-resmelt-save section atomic.
                        let _guard = CRUCIBLE_LOCK.lock().await;
                        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                        if let Some(ingot) = crucible.get(&id).cloned() {
                            let smith = crate::smith::make_base_smith(config, hooks);
                            if resmelt::resmelt_ingot(&mut crucible, &ingot, smith.as_ref()).await.is_ok() {
                                crucible.save()?;
                            } else {
                                crucible.set_status(&id, Status::Cracked);
                                crucible.save()?;
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
                return finish_run_over_budget(spent, cap, hooks).await;
            }

            // Show status
            let crucible = Crucible::load(Path::new(CRUCIBLE))?;
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
        let struck = strike_ingot(&ingot, smith.as_ref(), hooks).await;
        if let Err(SlagError::Cancelled) = struck {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            crucible.set_status(&ingot.id, Status::Ore);
            crucible.save()?;
            return Err(SlagError::Cancelled);
        }
        if let Err(SlagError::RunBudgetExhausted { spent, cap }) = struck {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            crucible.set_status(&ingot.id, Status::Ore);
            crucible.save()?;
            drop(_guard);
            return finish_run_over_budget(spent, cap, hooks).await;
        }
        if struck.is_ok() {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            crucible.set_status(&ingot.id, Status::Forged);
            crucible.save()?;
            emit(&hooks.events, EngineEvent::IngotDone { id: ingot.id.clone(), ok: true });
        } else {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            let base_smith = crate::smith::make_base_smith(config, hooks);
            if resmelt::resmelt_ingot(&mut crucible, &ingot, base_smith.as_ref()).await.is_ok() {
                // Re-smelted: status already updated by resmelt
                crucible.save()?;
            } else {
                crucible.set_status(&ingot.id, Status::Cracked);
                crucible.save()?;
                emit(&hooks.events, EngineEvent::IngotDone { id: ingot.id.clone(), ok: false });
            }
        }

        let crucible = Crucible::load(Path::new(CRUCIBLE))?;
        if !tui::is_quiet() {
            print!("\n  ");
            tui::ingot_status_line(&crucible.counts());
            println!();
        }
    }
}

/// Forge one solo ingot: multi-cast duel when the cast count says so,
/// otherwise (or on duel fallthrough/error) the normal single-smith
/// strike.
async fn forge_ingot(
    ingot: &Ingot,
    smith: &dyn Smith,
    duel_cfg: Option<(&EngineConfig, u8)>,
    hooks: &EngineHooks,
) -> Result<(), SlagError> {
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
            Box::new(
                crate::smith::native::NativeSmith::cast(
                    cfg.clone(),
                    ingot.skill.as_str(),
                    ingot.grade,
                    root.to_path_buf(),
                    &model,
                    hooks,
                )
                .with_ingot_spend(cast_spend.clone()),
            )
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
                if !tui::is_quiet() {
                    println!(
                        "    {}{}⚔█{} cast {winner} merged after {rounds} round(s)",
                        super::bold(),
                        super::fg(tui::PURE),
                        super::reset()
                    );
                }
                return Ok(());
            }
            Ok(duel::DuelOutcome::FellThrough) => {
                // Both casts failed a round: duel is off for this ingot,
                // fall through to the single-smith strike below.
            }
            Err(e @ (SlagError::Cancelled | SlagError::RunBudgetExhausted { .. })) => {
                return Err(e)
            }
            Err(e) => {
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
            if let Ok(crucible) = Crucible::load(Path::new(CRUCIBLE)) {
                if let Some(current) = crucible.get(&ingot.id) {
                    fallback.max = remaining_budget(ingot.max, current.heat);
                }
            }
        }
        if fallback.max == 0 {
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
        return strike_ingot(&fallback, fallback_smith.as_ref(), hooks).await;
    }
    strike_ingot(ingot, smith, hooks).await
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
) -> Result<(), SlagError> {
    let ore_left = {
        let _guard = CRUCIBLE_LOCK.lock().await;
        Crucible::load(Path::new(CRUCIBLE)).map(|c| c.counts().ore).unwrap_or(0)
    };
    let note = run_budget_note(spent, cap, ore_left);
    emit(&hooks.events, EngineEvent::Warning { message: note.clone() });
    if !tui::is_quiet() {
        println!("\n  {}⚠{} {note}", super::fg(tui::BRIGHT), super::reset());
    }
    append_assay_note(&note);
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
        .open(LEDGER)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(entry.as_bytes())
        });
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

    for heat in 1..=ingot.max {
        // Update heat in crucible file
        {
            let _guard = CRUCIBLE_LOCK.lock().await;
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            crucible.increment_heat(&ingot.id);
            crucible.save()?;
            let current = crucible.get(&ingot.id).map(|i| i.heat).unwrap_or(heat);
            emit(&hooks.events, EngineEvent::HeatTick { id: ingot.id.clone(), heat: current });
        }

        if !quiet {
            let hc = match heat {
                1..=2 => tui::WARM,
                3 => tui::HOT,
                4 => tui::BRIGHT,
                _ => tui::PURE,
            };
            let hb = if heat > 4 { super::bold() } else { String::new() };
            print!(
                "    {}{}⚒ {heat}/{}{} ",
                hb,
                super::fg(hc),
                ingot.max,
                super::reset()
            );
        }

        let flux_text = flux::prepare_flux(ingot, slag.as_deref());
        log_to_file(&format!("FLUX_{}_{heat}", ingot.id), &flux_text);

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

        let response = match invoke_absorbing_transients(
            smith,
            &flux_text,
            &ingot.id,
            MAX_TRANSIENT_PER_HEAT,
            hooks,
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
                slag = Some(format!("Smith error: {e}"));
                if !quiet {
                    println!("{}✗{}", super::fg(tui::WARM), super::reset());
                }
                continue;
            }
        };

        log_to_file(&format!("STRIKE_{}_{heat}", ingot.id), &response);

        // Extract CMD
        let cmd = match proof::extract_cmd(&response) {
            Some(c) => c,
            None => {
                slag = Some("NO CMD: line in response".into());
                if !quiet {
                    println!("{}✗{} no CMD", super::fg(tui::WARM), super::reset());
                }
                continue;
            }
        };

        if !quiet {
            print!("{}{}{} ", super::fg(tui::COLD), tui::truncate(&cmd, 32), super::reset());
            tui::flush();
        }

        // Run CMD
        let (ok, output) = proof::run_shell(&cmd).await;
        log_to_file(
            &format!("ASSAY_{}_{heat}", ingot.id),
            &format!("exit={}\n{output}", if ok { 0 } else { 1 }),
        );

        if ok {
            // Verify proof if different from cmd
            if !ingot.proof.is_empty() && ingot.proof != cmd && ingot.proof != "true" {
                let (proof_ok, proof_output) = proof::run_shell(&ingot.proof).await;
                if !proof_ok {
                    slag = Some(format!("Proof failed [{}]: {proof_output}", ingot.proof));
                    if !quiet {
                        println!("{}✗{} impure", super::fg(tui::WARM), super::reset());
                    }
                    continue;
                }
            }

            if !quiet {
                println!("{}{}█{}", super::bold(), super::fg(tui::PURE), super::reset());
            }
            proof::git_commit(&ingot.id, &ingot.work).await;
            append_ledger(ingot, heat);
            return Ok(());
        } else {
            slag = Some(format!("CMD failed (exit 1): {output}"));
            if !quiet {
                println!("{}✗{}", super::fg(tui::WARM), super::reset());
            }
        }
    }

    Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max))
}

fn append_ledger(ingot: &Ingot, heat: u8) {
    let entry = format!(
        "\n## {} [{}] gr:{} skill:{}\n- {}\n- heats:{}\n",
        chrono::Local::now().format("%m-%d %H:%M"),
        ingot.id,
        ingot.grade,
        ingot.skill,
        ingot.work,
        heat,
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(entry.as_bytes())
        });
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
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
