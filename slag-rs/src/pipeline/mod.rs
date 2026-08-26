pub mod surveyor;
pub mod founder;
pub mod forge;
pub mod duel;
pub mod resmelt;
pub mod assay;

use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};

use crate::config::EngineConfig;
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::smith::EngineHooks;
use crate::tui;

/// Paint a palette color for the stream-mode `print!` lines in this crate.
/// `tui::fg` is private, so callers route through `tui::downgrade` here
/// rather than hardcoding escape sequences that drift from the palette.
/// Returns an empty string when color is off, so redirected output and
/// `NO_COLOR` runs carry no escape bytes at all.
pub(crate) fn fg(color: Color) -> String {
    if !tui::colored() {
        return String::new();
    }
    SetForegroundColor(tui::downgrade(color)).to_string()
}

/// Clears color and attributes, matching what `\x1b[0m` used to do.
pub(crate) fn reset() -> String {
    if !tui::colored() {
        return String::new();
    }
    ResetColor.to_string()
}

/// Bold, or nothing when color is off. Pairs with `reset`.
pub(crate) fn bold() -> String {
    if !tui::colored() {
        return String::new();
    }
    SetAttribute(Attribute::Bold).to_string()
}

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

    // Item 36: a long unattended run should not die mid-ingot on an empty
    // account. Warn, never block: the balance endpoint is best-effort and
    // an unreachable one must not stop a forge.
    if let Some(floor) = crate::config::credit_floor() {
        if let Some(credits) =
            crate::engine::provider::fetch_credits(&config.api_key, &config.base_url).await
        {
            if credits.remaining() < floor {
                println!(
                    "  {}! OpenRouter balance ${:.2} is under the ${floor:.2} floor{}",
                    fg(tui::WARM),
                    credits.remaining(),
                    reset(),
                );
            }
        }
    }

    // Phase 1: Survey
    if !std::path::Path::new(crate::config::BLUEPRINT).exists() {
        let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Surveyor);
        surveyor::run(smith.as_ref()).await?;
    }

    // Phase 2: Found
    let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);
    let needs_founder = !crucible_path.exists() || {
        let content = std::fs::read_to_string(crucible_path).unwrap_or_default();
        !content.contains("(ingot ")
    };
    if needs_founder {
        let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Founder);
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

    let forged = forge::run(config, max_anvils, &hooks).await;

    // Phase 4: Assay. It runs on failure too — a run that cracked an ingot
    // is exactly when the user needs the counts and the cracked list, and
    // returning early left them with one line of "forge failed".
    if !matches!(forged, Err(SlagError::Cancelled)) {
        let _ = assay::show();
    }
    forged
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

    // Initial commit, scoped to the files slag just wrote. `git add -A`
    // here swept whatever the user had lying around — including .env —
    // into a commit they never asked for.
    let _ = std::process::Command::new("git")
        .args([
            "add",
            "--",
            ".gitignore",
            crate::config::ORE_FILE,
            crate::config::ALLOY_FILE,
            crate::config::LEDGER,
        ])
        .output();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "fire: furnace lit", "--quiet"])
        .output();

    tui::status_line("█", tui::HOT, "Furnace hot");
    Ok(())
}
