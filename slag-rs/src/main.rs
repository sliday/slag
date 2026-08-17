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
use config::EngineConfig;
use smith::EngineHooks;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Advertised isolation that silently does nothing corrupts trust (and
    // repos): until wired, say so loudly instead of ignoring the flag.
    if cli.worktree {
        eprintln!(
            "  \x1b[38;5;220m⚠\x1b[0m --worktree is not implemented yet; \
             all ingots run in the shared checkout"
        );
    }

    // Model flags override env; EngineConfig::load reads env downstream.
    // --auto first, so explicit model flags win over it.
    if cli.auto {
        std::env::set_var("SLAG_MODEL_BASE", "openrouter/auto");
        std::env::set_var("SLAG_MODEL_PLAN", "openrouter/auto");
        std::env::set_var("SLAG_MODEL_JUDGE", "openrouter/auto");
    }
    if let Some(m) = &cli.model {
        std::env::set_var("SLAG_MODEL_BASE", m);
    }
    if let Some(m) = &cli.plan_model {
        std::env::set_var("SLAG_MODEL_PLAN", m);
    }
    if let Some(m) = &cli.judge_model {
        std::env::set_var("SLAG_MODEL_JUDGE", m);
    }

    // Ensure logs directory exists
    let _ = std::fs::create_dir_all(config::LOG_DIR);

    // `status`, `update` and `key` inspect or repair a broken setup, so
    // none of them may demand the very key the user came here to fix.
    let result = match cli.command {
        Some(Command::Status) => show_status(),
        Some(Command::Update) => update::self_update().await,
        Some(Command::Key { key }) => run_key(key).await,
        Some(Command::Resume) => forge(None, cli.anvils, cli.tui).await,
        None => {
            let commission = cli.commission_text();
            forge(commission.as_deref(), cli.anvils, cli.tui).await
        }
    };

    if let Err(e) = result {
        eprintln!("\n  \x1b[31m✗\x1b[0m {e}\n");
        std::process::exit(1);
    }
}

/// Resolve the key (onboarding on first run) and forge. The key gate
/// runs before any project file is touched: a run that cannot call a
/// model should not leave a half-lit furnace behind.
async fn forge(
    commission: Option<&str>,
    anvils: usize,
    tui_flag: bool,
) -> Result<(), error::SlagError> {
    let config = EngineConfig::resolve().await?;
    run_pipeline(commission, &config, anvils, tui_flag).await
}

/// `slag key [KEY]` — the whole configuration surface. With a key it
/// verifies and saves; without one it reports what slag would use, or
/// onboards when there is nothing to report. Passing the key as an
/// argument leaks it into shell history, so the bare form prompts.
async fn run_key(key: Option<String>) -> Result<(), error::SlagError> {
    if let Some(key) = key {
        config::verify_and_store(key.trim()).await?;
        return Ok(());
    }

    let status = config::key_status();
    if status.is_none() && std::io::stdin().is_terminal() {
        config::onboard().await?;
        return Ok(());
    }
    let source = status
        .as_ref()
        .map(|(src, key)| (src.label(), config::mask_key(key)));

    // Models only resolve once a key exists; without one, show the defaults
    // the user will get rather than an empty panel.
    let models = match EngineConfig::load() {
        Some(cfg) => vec![
            ("work", cfg.model_base),
            ("plan", cfg.model_plan),
            ("duel", cfg.model_alt),
            ("judge", cfg.model_judge),
        ],
        None => ["work", "plan", "duel", "judge"]
            .iter()
            .map(|role| (*role, config::AUTO_MODEL.to_string()))
            .collect(),
    };
    let models: Vec<(&str, &str)> = models.iter().map(|(r, m)| (*r, m.as_str())).collect();

    tui::key_panel(source, &models);
    Ok(())
}

/// Run the pipeline, optionally under the full-screen dashboard.
/// `--tui` needs a real terminal on stdin for the key reader; headless
/// runs (CI, pipes) silently keep the stream-mode display.
async fn run_pipeline(
    commission: Option<&str>,
    config: &EngineConfig,
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
