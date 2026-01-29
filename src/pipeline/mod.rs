pub mod analysis;
pub mod assay;
pub mod forge;
pub mod founder;
pub mod resmelt;
pub mod review;
pub mod surveyor;

use crate::config::{PipelineConfig, SmithConfig};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::smith::claude::ClaudeSmith;
use crate::tui;

/// Run the full pipeline (4 or 5 phases depending on review mode).
pub async fn run(
    commission: Option<&str>,
    smith_config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<(), SlagError> {
    tui::show_banner();

    // Fire furnace if needed
    fire_furnace(commission)?;

    // Phase 1: Survey
    if !std::path::Path::new(crate::config::BLUEPRINT).exists() {
        let smith = ClaudeSmith::plan(smith_config);
        surveyor::run(&smith).await?;
    }

    // Phase 2: Found
    let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);
    let needs_founder = !crucible_path.exists() || {
        let content = std::fs::read_to_string(crucible_path).unwrap_or_default();
        !content.contains("(ingot ")
    };
    if needs_founder {
        let smith = ClaudeSmith::plan(smith_config);
        founder::run(&smith).await?;
    }

    // Phase 3: Forge (with retry loop)
    let forge_start = std::time::Instant::now();
    let mut cycle = 0;
    let max_cycles = pipeline_config.max_retry + 1; // +1 for initial attempt

    loop {
        cycle += 1;

        if cycle > 1 {
            tui::header(&format!("FORGE · retry {}/{}", cycle - 1, max_cycles - 1));
        } else {
            tui::header("FORGE");
        }
        tui::show_legend();

        let crucible = Crucible::load(crucible_path)?;
        let counts = crucible.counts();
        print!("  ");
        tui::ingot_status_line(&counts);
        println!();

        // Run forge (ignore ForgeFailed error - we handle it with analysis)
        let forge_result = forge::run(smith_config, pipeline_config).await;

        let forged_branches = match forge_result {
            Ok(branches) => branches,
            Err(SlagError::ForgeFailed(_)) => Vec::new(),
            Err(e) => return Err(e),
        };

        // Phase 3.5: Review (if worktree mode enabled)
        if pipeline_config.should_review() && !forged_branches.is_empty() {
            let smith = ClaudeSmith::base(smith_config);
            review::run(&smith, pipeline_config, &forged_branches).await?;
        }

        // Check if we're done (all forged, none cracked)
        let crucible = Crucible::load(crucible_path)?;
        let counts = crucible.counts();

        if counts.cracked == 0 {
            // Success!
            break;
        }

        // Check if we've exhausted retries
        if cycle >= max_cycles {
            println!(
                "\n  \x1b[31m✗\x1b[0m Max retries ({}) exhausted, {} ingots still cracked",
                max_cycles - 1,
                counts.cracked
            );
            break;
        }

        // Analyze failures and prepare for retry
        let smith = ClaudeSmith::base(smith_config);
        let can_retry = analysis::analyze_and_prepare(&smith, smith_config, cycle).await?;

        if !can_retry {
            println!("\n  \x1b[31m✗\x1b[0m No recoverable ingots, stopping");
            break;
        }

        println!("\n  \x1b[38;5;220m↺\x1b[0m Retrying forge...\n");
    }

    // Phase 4: Assay
    let elapsed_secs = forge_start.elapsed().as_secs();
    assay::show(Some(elapsed_secs))?;

    // Final check - if any cracked, return error
    let crucible = Crucible::load(crucible_path)?;
    let counts = crucible.counts();
    if counts.cracked > 0 {
        return Err(SlagError::ForgeFailed(counts.cracked));
    }

    Ok(())
}

/// Initialize project structure (fire the furnace)
fn fire_furnace(commission: Option<&str>) -> Result<(), SlagError> {
    let ore_path = std::path::Path::new(crate::config::ORE_FILE);

    if ore_path.exists() {
        return Ok(());
    }

    let commission = commission.ok_or(SlagError::NoOre)?;

    tui::header("FIRING FURNACE");

    // git init
    let _ = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .output();

    // .gitignore
    let gitignore = std::path::Path::new(".gitignore");
    let content = std::fs::read_to_string(gitignore).unwrap_or_default();
    if !content.contains("logs/") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(gitignore)?;
        use std::io::Write;
        writeln!(f, "logs/")?;
    }

    // Create PRD.md
    std::fs::write(ore_path, format!("# Commission\n\n{commission}\n"))?;
    tui::status_line("░", tui::COLD, "Ore loaded");

    // Create AGENTS.md
    let alloy_path = std::path::Path::new(crate::config::ALLOY_FILE);
    if !alloy_path.exists() {
        std::fs::write(alloy_path, "## Alloy Recipes\n")?;
        tui::status_line("+", tui::COLD, "Recipes ready");
    }

    // Create PROGRESS.md
    let ledger_path = std::path::Path::new(crate::config::LEDGER);
    if !ledger_path.exists() {
        std::fs::write(
            ledger_path,
            format!(
                "# Smithy Ledger\nFired: {}\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
        )?;
        tui::status_line("+", tui::COLD, "Ledger open");
    }

    // Create logs dir
    std::fs::create_dir_all(crate::config::LOG_DIR)?;

    // Initial commit
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "fire: furnace lit", "--quiet"])
        .output();

    tui::status_line("█", tui::HOT, "Furnace hot");
    Ok(())
}
