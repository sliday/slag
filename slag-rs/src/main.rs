mod anvil;
mod cli;
mod config;
mod crucible;
mod dashboard;
mod engine;
mod error;
mod flux;
mod pipeline;
mod progress;
mod proof;
mod sexp;
mod smith;
mod tui;
mod update;

use std::io::IsTerminal;
use std::path::Path;

use clap::Parser;

use cli::{Cli, Command};
use config::SmithConfig;
use smith::EngineHooks;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Ensure logs directory exists
    let _ = std::fs::create_dir_all(config::LOG_DIR);

    let result = match cli.command {
        Some(Command::Status) => show_status(),
        Some(Command::Update) => update::self_update().await,
        Some(Command::Resume) => {
            let config = SmithConfig::from_env();
            match ensure_engine_key() {
                Ok(()) => run_pipeline(None, &config, cli.anvils, cli.tui).await,
                Err(e) => Err(e),
            }
        }
        None => {
            let config = SmithConfig::from_env();
            match ensure_engine_key() {
                Ok(()) => {
                    let commission = cli.commission_text();
                    run_pipeline(commission.as_deref(), &config, cli.anvils, cli.tui).await
                }
                Err(e) => Err(e),
            }
        }
    };

    if let Err(e) = result {
        eprintln!("\n  \x1b[31m✗\x1b[0m {e}\n");
        std::process::exit(1);
    }
}

/// Run the pipeline, optionally under the full-screen dashboard.
/// `--tui` needs a real terminal on stdin for the key reader; headless
/// runs (CI, pipes) silently keep the stream-mode display.
async fn run_pipeline(
    commission: Option<&str>,
    config: &SmithConfig,
    anvils: usize,
    tui_flag: bool,
) -> Result<(), error::SlagError> {
    if !(tui_flag && std::io::stdin().is_terminal()) {
        return pipeline::run(commission, config, anvils, EngineHooks::default()).await;
    }

    let (tx, rx) = engine::events::channel();
    let steer = engine::SteerQueue::default();
    let cancel = engine::CancelFlag::default();
    let hooks = EngineHooks {
        events: Some(tx),
        steer: Some(steer.clone()),
        cancel: Some(cancel.clone()),
    };

    tui::set_quiet(true);
    let dash = tokio::spawn(dashboard::run(rx, steer, cancel));

    let result = pipeline::run(commission, config, anvils, hooks.clone()).await;

    // Surface pipeline-level failures (crucible parse, ForgeFailed, IO)
    // in the dashboard feed; otherwise the run ends with no visible
    // signal and the app looks hung.
    if let Err(e) = &result {
        engine::emit(
            &hooks.events,
            engine::EngineEvent::Error { message: format!("pipeline stopped: {e}") },
        );
    }

    // Drop every EventTx so the dashboard drains its channel; it stays up
    // for review until the user detaches (q/Esc), then restores the
    // terminal and un-quiets the stream tui itself.
    drop(hooks);
    let _ = dash.await;
    tui::set_quiet(false);

    // The alternate screen took the in-dashboard ASSAY output with it;
    // reprint the final report on the real terminal.
    if result.is_ok() {
        let _ = pipeline::assay::show();
    }

    result
}

/// OpenRouter key is a prerequisite: prompt on first run.
/// `SLAG_SMITH` env stays as the legacy CLI-smith escape hatch.
fn ensure_engine_key() -> Result<(), error::SlagError> {
    if config::EngineConfig::load().is_some() || std::env::var_os("SLAG_SMITH").is_some() {
        return Ok(());
    }
    config::prompt_for_key().map(|_| ())
}

fn show_status() -> Result<(), error::SlagError> {
    tui::show_banner();

    let crucible_path = Path::new(config::CRUCIBLE);
    if !crucible_path.exists() {
        println!("\n  No crucible found. Run `slag \"Your Commission\"` to start.\n");
        return Ok(());
    }

    let crucible = crucible::Crucible::load(crucible_path)?;
    let counts = crucible.counts();

    let ore_path = Path::new(config::ORE_FILE);
    if ore_path.exists() {
        let ore = std::fs::read_to_string(ore_path)?;
        let commission = ore.lines().last().unwrap_or("(unknown)");
        println!(
            "\n  \x1b[38;5;208mCommission:\x1b[0m {}",
            tui::truncate(commission, 50)
        );
    }

    let has_bp = Path::new(config::BLUEPRINT).exists();
    println!("  \x1b[90mBlueprint: {}\x1b[0m", if has_bp { "yes" } else { "no" });

    print!("  ");
    tui::ingot_status_line(&counts);
    println!();
    tui::temper_bar(&counts);

    if counts.cracked > 0 {
        println!("\n  \x1b[31mCracked:\x1b[0m");
        for ingot in &crucible.ingots {
            if ingot.status == sexp::Status::Cracked {
                println!("    \x1b[31m✗\x1b[0m [{}] {}", ingot.id, ingot.work);
            }
        }
    }

    println!();
    Ok(())
}
