use std::path::Path;

use crate::config::{EngineConfig, CRUCIBLE, LEDGER};
use crate::crucible::{Crucible, CRUCIBLE_LOCK};
use crate::engine::provider::OpenRouter;
use crate::engine::{emit, EngineEvent};
use crate::error::SlagError;
use crate::flux;
use crate::proof;
use crate::sexp::{Ingot, Status};
use crate::smith::{EngineHooks, Smith};
use crate::tui;

use super::{duel, resmelt};

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
            // Check for cracked
            let counts = crucible.counts();
            if counts.cracked > 0 {
                return Err(SlagError::ForgeFailed(counts.cracked));
            }
            return Ok(());
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
                    "\n  \x1b[38;5;208m⚒\x1b[38;5;220m⚒\x1b[1;37m⚒\x1b[0m \x1b[90m{} anvils:\x1b[0m \x1b[1;37m{}\x1b[0m",
                    solo_ids.len(),
                    solo_ids.join(" "),
                );
            }

            // Spawn parallel tasks
            let mut set = tokio::task::JoinSet::new();
            for ingot in ingot_snapshots {
                let smith = crate::smith::make_smith(config, ingot.skill.as_str(), ingot.grade, hooks);
                let task_hooks = hooks.clone();
                let duel_cfg = duel::should_duel(config, &ingot).then(|| config.clone());
                set.spawn(async move {
                    emit(
                        &task_hooks.events,
                        EngineEvent::IngotStart { id: ingot.id.clone(), work: ingot.work.clone() },
                    );
                    let result =
                        forge_ingot(&ingot, smith.as_ref(), duel_cfg.as_ref(), &task_hooks).await;
                    (ingot.id.clone(), result)
                });
            }

            // Collect results and update crucible on main thread
            let mut cancelled = false;
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
                            eprintln!("  \x1b[31m✗\x1b[0m anvil panicked: {e}");
                        }
                    }
                }
            }

            if cancelled {
                return Err(SlagError::Cancelled);
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

/// Forge one solo ingot: twin-cast duel when configured, otherwise (or on
/// duel fallthrough/error) the normal single-smith strike.
async fn forge_ingot(
    ingot: &Ingot,
    smith: &dyn Smith,
    duel_cfg: Option<&EngineConfig>,
    hooks: &EngineHooks,
) -> Result<(), SlagError> {
    if let Some(cfg) = duel_cfg {
        let casts = |cast: char, root: &Path| -> Box<dyn Smith> {
            let (model, cfg) = if cast == 'a' {
                (cfg.model_base.clone(), cfg.clone())
            } else {
                (cfg.model_alt.clone(), cfg.clone())
            };
            Box::new(crate::smith::native::NativeSmith::cast(
                cfg,
                ingot.skill.as_str(),
                ingot.grade,
                root.to_path_buf(),
                &model,
                hooks,
            ))
        };
        let judge_provider =
            OpenRouter::with_base_url(cfg.api_key.clone(), cfg.base_url.clone());

        match duel::duel_ingot(Path::new("."), ingot, cfg, hooks, &casts, &judge_provider).await {
            Ok(duel::DuelOutcome::Merged { winner, rounds }) => {
                if !tui::is_quiet() {
                    println!(
                        "    \x1b[1;37m⚔█\x1b[0m cast {winner} merged after {rounds} round(s)"
                    );
                }
                return Ok(());
            }
            Ok(duel::DuelOutcome::FellThrough) => {
                // Both casts failed a round: duel is off for this ingot,
                // fall through to the single-smith strike below.
            }
            Err(SlagError::Cancelled) => return Err(SlagError::Cancelled),
            Err(e) => {
                if !tui::is_quiet() {
                    println!("    \x1b[31m⚔✗\x1b[0m duel failed ({e}) — single-smith fallback");
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
        return strike_ingot(&fallback, smith, hooks).await;
    }
    strike_ingot(ingot, smith, hooks).await
}

/// Heats left after `spent` of a `max` budget.
fn remaining_budget(max: u8, spent: u8) -> u8 {
    max.saturating_sub(spent)
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
            "\n  \x1b[38;5;208m▣\x1b[0m \x1b[1;37m[{}]\x1b[0m {}{}{}",
            ingot.id,
            tui::truncate(&ingot.work, 42),
            if ingot.is_complex() { " \x1b[38;5;220m◉\x1b[0m" } else { "" },
            if ingot.is_web() { " \x1b[38;5;208m⚡\x1b[0m" } else { "" },
        );
        println!(
            "    \x1b[90mgr:{} skill:{} proof:{}\x1b[0m",
            ingot.grade,
            ingot.skill,
            tui::truncate(&ingot.proof, 30),
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
                1..=2 => "\x1b[31m",
                3 => "\x1b[38;5;208m",
                4 => "\x1b[38;5;220m",
                _ => "\x1b[1;37m",
            };
            print!("    {hc}⚒ {heat}/{}\x1b[0m ", ingot.max);
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
        let spinner = tui::spinner(spinner_msg);

        let response = match smith.invoke(&flux_text).await {
            Ok(r) => {
                spinner.finish_and_clear();
                r
            }
            Err(e) => {
                spinner.finish_and_clear();
                // Ctrl-C cancellation is not a retryable smith failure:
                // retrying burns the whole heat budget on instant errors
                // and cracks the ingot. Propagate so the run stops.
                if matches!(e, SlagError::Cancelled) {
                    return Err(e);
                }
                slag = Some(format!("Smith error: {e}"));
                if !quiet {
                    println!("\x1b[31m✗\x1b[0m");
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
                    println!("\x1b[31m✗\x1b[0m no CMD");
                }
                continue;
            }
        };

        if !quiet {
            print!("\x1b[90m{}\x1b[0m ", tui::truncate(&cmd, 32));
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
                        println!("\x1b[31m✗\x1b[0m impure");
                    }
                    continue;
                }
            }

            if !quiet {
                println!("\x1b[1;37m█\x1b[0m");
            }
            proof::git_commit(&ingot.id, &ingot.work).await;
            append_ledger(ingot, heat);
            return Ok(());
        } else {
            slag = Some(format!("CMD failed (exit 1): {output}"));
            if !quiet {
                println!("\x1b[31m✗\x1b[0m");
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
    use super::remaining_budget;

    #[test]
    fn remaining_budget_never_exceeds_max_or_underflows() {
        assert_eq!(remaining_budget(5, 0), 5);
        assert_eq!(remaining_budget(5, 2), 3);
        assert_eq!(remaining_budget(5, 5), 0);
        // Duel rounds may overshoot the budget; no wrap-around.
        assert_eq!(remaining_budget(5, 7), 0);
    }
}
