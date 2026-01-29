use std::path::Path;

use crate::anvil::worktree;
use crate::config::{PipelineConfig, SmithConfig, CRUCIBLE, LEDGER};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::flux;
use crate::proof;
use crate::sexp::{Ingot, Status};
use crate::smith::claude::ClaudeSmith;
use crate::smith::Smith;
use crate::tui;

use super::resmelt;

/// Result of forging an ingot, including branch name if worktree mode
#[derive(Debug, Clone)]
pub struct ForgeResult {
    pub id: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
}

/// Phase 3: Forge loop — parallel anvils then sequential
/// Returns list of forged branches (empty if not using worktree mode)
pub async fn run(
    config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<Vec<ForgeResult>, SlagError> {
    let mut forged_results: Vec<ForgeResult> = Vec::new();
    let use_worktree = pipeline_config.worktree;
    let max_anvils = pipeline_config.max_anvils;

    loop {
        let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;

        if !crucible.has_pending() {
            // Check for cracked
            let counts = crucible.counts();
            if counts.cracked > 0 {
                return Err(SlagError::ForgeFailed(counts.cracked));
            }
            return Ok(forged_results);
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

            println!("\n  \x1b[38;5;208m⚒ ANVILS [{}]\x1b[0m", solo_ids.len());
            let last_idx = ingot_snapshots.len().saturating_sub(1);
            for (i, ingot) in ingot_snapshots.iter().enumerate() {
                let prefix = if i == last_idx { "└─" } else { "├─" };
                println!(
                    "  \x1b[90m{}\x1b[0m \x1b[1;37m{}\x1b[0m  \x1b[38;5;208m◐\x1b[0m forging...  \x1b[90m{}\x1b[0m",
                    prefix,
                    ingot.id,
                    tui::truncate(&ingot.work, 40),
                );
            }

            // Spawn parallel tasks
            let mut set = tokio::task::JoinSet::new();
            for ingot in ingot_snapshots {
                let smith_cmd = config.select(ingot.skill.as_str(), ingot.grade).to_string();
                let worktree_mode = use_worktree;
                set.spawn(async move {
                    let smith = ClaudeSmith::new(smith_cmd);
                    let result = strike_ingot(&ingot, &smith, worktree_mode).await;
                    (ingot.id.clone(), result)
                });
            }

            // Collect results and update crucible on main thread
            while let Some(result) = set.join_next().await {
                let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                match result {
                    Ok((id, Ok(forge_result))) => {
                        crucible.set_status(&id, Status::Forged);
                        crucible.save()?;
                        forged_results.push(forge_result);
                    }
                    Ok((id, Err(_))) => {
                        // Try resmelt
                        if let Some(ingot) = crucible.get(&id).cloned() {
                            let smith = ClaudeSmith::base(config);
                            if resmelt::resmelt_ingot(&mut crucible, &ingot, &smith)
                                .await
                                .is_ok()
                            {
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

        match strike_ingot(&ingot, &smith, use_worktree).await {
            Ok(forge_result) => {
                let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                crucible.set_status(&ingot.id, Status::Forged);
                crucible.save()?;
                forged_results.push(forge_result);
            }
            Err(_) => {
                let mut crucible = Crucible::load(Path::new(CRUCIBLE))?;
                let base_smith = ClaudeSmith::base(config);
                if resmelt::resmelt_ingot(&mut crucible, &ingot, &base_smith)
                    .await
                    .is_ok()
                {
                    // Re-smelted: status already updated by resmelt
                    crucible.save()?;
                } else {
                    crucible.set_status(&ingot.id, Status::Cracked);
                    crucible.save()?;
                }
            }
        }

        let crucible = Crucible::load(Path::new(CRUCIBLE))?;
        print!("\n  ");
        tui::ingot_status_line(&crucible.counts());
        println!();
    }
}

/// Strike a single ingot: retry with heat, extract CMD, verify proof.
/// If worktree_mode is true, creates an isolated worktree branch for the work.
async fn strike_ingot(
    ingot: &Ingot,
    smith: &dyn Smith,
    worktree_mode: bool,
) -> Result<ForgeResult, SlagError> {
    let mut slag: Option<String> = None;
    let mut worktree_path: Option<String> = None;
    let branch_name = format!("forge/{}", ingot.id);

    // Create worktree if in worktree mode
    if worktree_mode {
        match worktree::create(&ingot.id).await {
            Ok(path) => {
                worktree_path = Some(path.clone());
                println!(
                    "    \x1b[90m↳ worktree: {}\x1b[0m",
                    tui::truncate(&path, 40)
                );
            }
            Err(e) => {
                eprintln!("    \x1b[31m✗\x1b[0m worktree create failed: {e}");
                return Err(e);
            }
        }
    }

    println!(
        "\n  \x1b[38;5;208m▣\x1b[0m \x1b[1;37m[{}]\x1b[0m {}{}{}",
        ingot.id,
        tui::truncate(&ingot.work, 42),
        if ingot.is_complex() {
            " \x1b[38;5;220m◉\x1b[0m"
        } else {
            ""
        },
        if ingot.is_web() {
            " \x1b[38;5;208m⚡\x1b[0m"
        } else {
            ""
        },
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
        print!(
            "    {hc}{} {heat}/{}\x1b[0m ",
            tui::heat_bar(heat, ingot.max),
            ingot.max
        );

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

        // In worktree mode, invoke smith in the worktree directory
        let response = if let Some(ref wt_path) = worktree_path {
            invoke_smith_in_worktree(smith, &flux_text, wt_path).await
        } else {
            smith.invoke(&flux_text).await
        };

        let response = match response {
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
                println!("\x1b[31m✗\x1b[0m smith output missing \"CMD:\" line");
                continue;
            }
        };

        print!("\x1b[90m{}\x1b[0m ", tui::truncate(&cmd, 32));
        tui::flush();

        // Run CMD (in worktree if applicable)
        let (ok, output) = if let Some(ref wt_path) = worktree_path {
            run_shell_in_dir(&cmd, wt_path).await
        } else {
            proof::run_shell(&cmd).await
        };
        log_to_file(
            &format!("ASSAY_{}_{heat}", ingot.id),
            &format!("exit={}\n{output}", if ok { 0 } else { 1 }),
        );

        if ok {
            // Verify proof if different from cmd
            if !ingot.proof.is_empty() && ingot.proof != cmd && ingot.proof != "true" {
                let (proof_ok, proof_output) = if let Some(ref wt_path) = worktree_path {
                    run_shell_in_dir(&ingot.proof, wt_path).await
                } else {
                    proof::run_shell(&ingot.proof).await
                };
                if !proof_ok {
                    slag = Some(format!("Proof failed [{}]: {proof_output}", ingot.proof));
                    println!(
                        "\x1b[31m✗\x1b[0m proof failed: {} (exit 1)",
                        tui::truncate(&ingot.proof, 30)
                    );
                    continue;
                }
            }

            println!("\x1b[1;37m█\x1b[0m");

            // Commit in worktree or main repo
            if let Some(ref wt_path) = worktree_path {
                git_commit_in_dir(&ingot.id, &ingot.work, wt_path).await;
            } else {
                proof::git_commit(&ingot.id, &ingot.work).await;
            }

            append_ledger(ingot, heat);
            return Ok(ForgeResult {
                id: ingot.id.clone(),
                branch: if worktree_mode {
                    Some(branch_name)
                } else {
                    None
                },
                worktree_path,
            });
        } else {
            slag = Some(format!("CMD failed (exit 1): {output}"));
            println!("\x1b[31m✗\x1b[0m");
        }
    }

    // Clean up worktree on failure (preserve branch for debugging)
    if worktree_path.is_some() {
        worktree::cleanup_without_merge(&ingot.id).await;
    }

    Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max))
}

/// Invoke smith in a specific directory (worktree)
async fn invoke_smith_in_worktree(
    smith: &dyn Smith,
    prompt: &str,
    worktree_path: &str,
) -> Result<String, SlagError> {
    // The smith will work in the current directory, so we need to
    // modify the prompt to include worktree context
    let enhanced_prompt = format!(
        "WORKTREE: You are working in an isolated git worktree at: {worktree_path}\n\
        All file operations should be relative to this directory.\n\n\
        {prompt}"
    );
    smith.invoke(&enhanced_prompt).await
}

/// Run a shell command in a specific directory
async fn run_shell_in_dir(cmd: &str, dir: &str) -> (bool, String) {
    match tokio::process::Command::new("bash")
        .args(["-c", cmd])
        .current_dir(dir)
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            (output.status.success(), combined)
        }
        Err(e) => (false, format!("spawn error: {e}")),
    }
}

/// Git commit in a specific directory (worktree)
async fn git_commit_in_dir(id: &str, work: &str, dir: &str) {
    let msg = format!("forge({id}): {work}");
    let _ = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .await;
    let _ = tokio::process::Command::new("git")
        .args(["commit", "-m", &msg, "--quiet"])
        .current_dir(dir)
        .output()
        .await;
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
