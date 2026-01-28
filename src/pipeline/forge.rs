use std::path::Path;

use crate::config::{SmithConfig, CRUCIBLE, LEDGER};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::flux;
use crate::proof;
use crate::sexp::{Ingot, Status};
use crate::smith::claude::ClaudeSmith;
use crate::smith::Smith;
use crate::tui;

use super::resmelt;

/// Phase 3: Forge loop — parallel anvils then sequential
pub async fn run(config: &SmithConfig, max_anvils: usize) -> Result<(), SlagError> {
    loop {
        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;

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

            println!(
                "\n  \x1b[38;5;208m⚒\x1b[38;5;220m⚒\x1b[1;37m⚒\x1b[0m \x1b[90m{} anvils:\x1b[0m \x1b[1;37m{}\x1b[0m",
                solo_ids.len(),
                solo_ids.join(" "),
            );

            // Spawn parallel tasks
            let mut set = tokio::task::JoinSet::new();
            for ingot in ingot_snapshots {
                let smith_cmd = config.select(ingot.skill.as_str(), ingot.grade).to_string();
                set.spawn(async move {
                    let smith = ClaudeSmith::new(smith_cmd);
                    let result = strike_ingot(&ingot, &smith).await;
                    (ingot.id.clone(), result)
                });
            }

            // Collect results and update crucible on main thread
            while let Some(result) = set.join_next().await {
                let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                match result {
                    Ok((id, Ok(()))) => {
                        crucible.set_status(&id, Status::Forged);
                        crucible.save()?;
                    }
                    Ok((id, Err(_))) => {
                        // Try resmelt
                        if let Some(ingot) = crucible.get(&id).cloned() {
                            let smith = ClaudeSmith::base(config);
                            if resmelt::resmelt_ingot(&mut crucible, &ingot, &smith).await.is_ok() {
                                crucible.save()?;
                            } else {
                                crucible.set_status(&id, Status::Cracked);
                                crucible.save()?;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  \x1b[31m✗\x1b[0m anvil panicked: {e}");
                    }
                }
            }

            // Show status
            let crucible = Crucible::load(Path::new(CRUCIBLE))?;
            print!("\n  ");
            tui::ingot_status_line(&crucible.counts());
            println!();
            continue;
        }

        // --- Sequential for :solo nil ---
        let ingot = match crucible.next_ore() {
            Some(i) => i.clone(),
            None => continue,
        };

        crucible.set_status(&ingot.id, Status::Molten);
        crucible.save()?;

        let smith_cmd = config.select(ingot.skill.as_str(), ingot.grade).to_string();
        let smith = ClaudeSmith::new(smith_cmd);

        if strike_ingot(&ingot, &smith).await.is_ok() {
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            crucible.set_status(&ingot.id, Status::Forged);
            crucible.save()?;
        } else {
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            let base_smith = ClaudeSmith::base(config);
            if resmelt::resmelt_ingot(&mut crucible, &ingot, &base_smith).await.is_ok() {
                // Re-smelted: status already updated by resmelt
                crucible.save()?;
            } else {
                crucible.set_status(&ingot.id, Status::Cracked);
                crucible.save()?;
            }
        }

        let crucible = Crucible::load(Path::new(CRUCIBLE))?;
        print!("\n  ");
        tui::ingot_status_line(&crucible.counts());
        println!();
    }
}

/// Strike a single ingot: retry with heat, extract CMD, verify proof.
async fn strike_ingot(ingot: &Ingot, smith: &dyn Smith) -> Result<(), SlagError> {
    let mut slag: Option<String> = None;

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

    for heat in 1..=ingot.max {
        // Update heat in crucible file
        {
            let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
            crucible.increment_heat(&ingot.id);
            crucible.save()?;
        }

        let hc = match heat {
            1..=2 => "\x1b[31m",
            3 => "\x1b[38;5;208m",
            4 => "\x1b[38;5;220m",
            _ => "\x1b[1;37m",
        };
        print!("    {hc}⚒ {heat}/{}\x1b[0m ", ingot.max);

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
                slag = Some(format!("Smith error: {e}"));
                println!("\x1b[31m✗\x1b[0m");
                continue;
            }
        };

        log_to_file(&format!("STRIKE_{}_{heat}", ingot.id), &response);

        // Extract CMD
        let cmd = match proof::extract_cmd(&response) {
            Some(c) => c,
            None => {
                slag = Some("NO CMD: line in response".into());
                println!("\x1b[31m✗\x1b[0m no CMD");
                continue;
            }
        };

        print!("\x1b[90m{}\x1b[0m ", tui::truncate(&cmd, 32));
        tui::flush();

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
                    println!("\x1b[31m✗\x1b[0m impure");
                    continue;
                }
            }

            println!("\x1b[1;37m█\x1b[0m");
            proof::git_commit(&ingot.id, &ingot.work).await;
            append_ledger(ingot, heat);
            return Ok(());
        } else {
            slag = Some(format!("CMD failed (exit 1): {output}"));
            println!("\x1b[31m✗\x1b[0m");
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
