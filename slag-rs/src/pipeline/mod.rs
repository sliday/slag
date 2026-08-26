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
    let ore = fire_furnace(commission)?;

    // Item 36: a long unattended run should not die mid-ingot on an empty
    // account. Warn, never block: the balance endpoint is best-effort and
    // an unreachable one must not stop a forge.
    if let Some(floor) = crate::config::credit_floor() {
        if let Some(credits) =
            crate::engine::provider::fetch_credits(&config.api_key, &config.base_url).await
        {
            if credits.remaining() < floor {
                // Same rule as the addendum notice: an unguarded print
                // corrupts the dashboard, so this goes through the stream
                // and the stream renders it wherever the user is looking.
                crate::engine::emit(
                    &hooks.events,
                    crate::engine::EngineEvent::Warning {
                        message: format!(
                            "OpenRouter balance ${:.2} is under the ${floor:.2} floor",
                            credits.remaining()
                        ),
                    },
                );
            }
        }
    }

    // Phase 1: Survey. An extended ore re-surveys: the blueprint the
    // founder reads must describe the project the addendum asked for, not
    // the one before it.
    if ore == Ore::Extended || !std::path::Path::new(crate::config::BLUEPRINT).exists() {
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
    } else if ore == Ore::Extended {
        let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Founder);
        let added = founder::extend(smith.as_ref()).await?;
        if added == 0 {
            // The model read the addendum and found nothing to build.
            // Saying so beats a silent finish that looks like a no-op.
            // It rides the event stream rather than stdout: a bare
            // `println!` under --tui writes into the alternate screen and
            // lands glued to whatever row is being drawn.
            crate::engine::emit(
                &hooks.events,
                crate::engine::EngineEvent::Warning {
                    message: "the addendum cast no ingots — nothing in it needs building"
                        .to_string(),
                },
            );
        }
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
/// What `fire_furnace` did with the commission it was handed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ore {
    /// The project already existed and no new commission came with it.
    Unchanged,
    /// A fresh project: PRD.md written from the commission.
    Fired,
    /// A live project got a second commission, appended to the ore. The
    /// blueprint and the crucible both need to catch up.
    Extended,
}

fn fire_furnace(commission: Option<&str>) -> Result<Ore, SlagError> {
    let ore_path = std::path::Path::new(crate::config::ORE_FILE);

    if ore_path.exists() {
        // A commission for a project that already has ore used to be
        // dropped on the floor: no error, no warning, and a run that
        // looked like it did nothing. Append it instead, and let the
        // survey and the founder work out what it adds.
        let Some(commission) = commission.filter(|c| !c.trim().is_empty()) else {
            return Ok(Ore::Unchanged);
        };
        let existing = std::fs::read_to_string(ore_path).unwrap_or_default();
        if existing.contains(commission.trim()) {
            // Re-running the same commission is not a new request; it is
            // usually a repeated command. Say so rather than re-planning.
            return Ok(Ore::Unchanged);
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(ore_path)?;
        writeln!(
            f,
            "\n## Addendum — {}\n\n{}",
            chrono::Local::now().format("%Y-%m-%d %H:%M"),
            commission.trim()
        )?;
        tui::status_line("+", tui::HOT, "Ore extended");
        return Ok(Ore::Extended);
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
    Ok(Ore::Fired)
}

#[cfg(test)]
mod ore_tests {
    use super::*;

    fn in_dir<T>(f: impl FnOnce() -> T) -> T {
        // fire_furnace works on relative paths, so each case needs its own
        // directory. The lock keeps concurrent tests from sharing a cwd.
        static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prior = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let out = f();
        std::env::set_current_dir(prior).unwrap();
        out
    }

    #[test]
    fn a_commission_for_a_live_project_extends_the_ore() {
        // The bug: this returned Ok and dropped the commission, so the run
        // surveyed nothing, founded nothing, and looked like a no-op.
        in_dir(|| {
            std::fs::write(crate::config::ORE_FILE, "# Commission\n\nBuild a shooter.\n").unwrap();
            let ore = fire_furnace(Some("add a leaderboard")).unwrap();
            assert_eq!(ore, Ore::Extended);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert!(prd.contains("Build a shooter."), "the original ore survives: {prd}");
            assert!(prd.contains("## Addendum"), "{prd}");
            assert!(prd.contains("add a leaderboard"), "{prd}");
        });
    }

    #[test]
    fn no_commission_on_a_live_project_changes_nothing() {
        in_dir(|| {
            std::fs::write(crate::config::ORE_FILE, "# Commission\n\nBuild a shooter.\n").unwrap();
            assert_eq!(fire_furnace(None).unwrap(), Ore::Unchanged);
            assert_eq!(fire_furnace(Some("   ")).unwrap(), Ore::Unchanged);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert!(!prd.contains("Addendum"), "an empty commission is not a request: {prd}");
        });
    }

    #[test]
    fn re_running_the_same_commission_is_not_a_new_request() {
        // Pressing Up+Enter, or re-running a shell command, must not stack
        // duplicate addenda and re-plan the same work.
        in_dir(|| {
            std::fs::write(crate::config::ORE_FILE, "# Commission\n\nBuild a shooter.\n").unwrap();
            assert_eq!(fire_furnace(Some("add a leaderboard")).unwrap(), Ore::Extended);
            assert_eq!(fire_furnace(Some("add a leaderboard")).unwrap(), Ore::Unchanged);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert_eq!(prd.matches("add a leaderboard").count(), 1, "{prd}");
        });
    }

    #[test]
    fn a_cold_directory_still_fires_the_furnace() {
        in_dir(|| {
            assert_eq!(fire_furnace(Some("Build a shooter")).unwrap(), Ore::Fired);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert!(prd.contains("Build a shooter"), "{prd}");
            assert!(!prd.contains("Addendum"), "a first commission is the ore, not an addendum");
        });
    }

    #[test]
    fn a_cold_directory_with_no_commission_still_asks_for_one() {
        in_dir(|| {
            assert!(matches!(fire_furnace(None), Err(SlagError::NoOre)));
        });
    }
}
