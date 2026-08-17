pub mod surveyor;
pub mod founder;
pub mod forge;
pub mod duel;
pub mod resmelt;
pub mod assay;

use crate::config::EngineConfig;
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::smith::EngineHooks;
use crate::tui;

/// Run the full 4-phase pipeline.
pub async fn run(
    commission: Option<&str>,
    config: &EngineConfig,
    max_anvils: usize,
    hooks: EngineHooks,
) -> Result<(), SlagError> {
    tui::show_banner();

    // Fire furnace if needed
    fire_furnace(commission)?;

    // Phase 1: Survey
    if !std::path::Path::new(crate::config::BLUEPRINT).exists() {
        let smith = crate::smith::make_plan_smith(config, &hooks);
        surveyor::run(smith.as_ref()).await?;
    }

    // Phase 2: Found
    let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);
    let needs_founder = !crucible_path.exists() || {
        let content = std::fs::read_to_string(crucible_path).unwrap_or_default();
        !content.contains("(ingot ")
    };
    if needs_founder {
        let smith = crate::smith::make_plan_smith(config, &hooks);
        founder::run(smith.as_ref()).await?;
    }

    // Phase 3: Forge
    tui::header("FORGE");
    let crucible = Crucible::load(crucible_path)?;
    let counts = crucible.counts();
    if !tui::is_quiet() {
        print!("  ");
        tui::ingot_status_line(&counts);
        println!();
    }

    forge::run(config, max_anvils, &hooks).await?;

    // Phase 4: Assay
    assay::show()?;

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
    std::fs::write(
        ore_path,
        format!("# Commission\n\n{commission}\n"),
    )?;
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
            format!("# Smithy Ledger\nFired: {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M")),
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
