#![allow(dead_code)]

mod anvil;
mod cli;
mod config;
mod crucible;
mod error;
mod flux;
mod pipeline;
mod progress;
mod proof;
mod sexp;
mod smith;
mod tui;
mod update;

use std::path::Path;

use clap::Parser;

use cli::{Cli, Command};
use config::{PipelineConfig, SmithConfig};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Ensure logs directory exists
    let _ = std::fs::create_dir_all(config::LOG_DIR);

    let pipeline_config = PipelineConfig::new(
        cli.worktree,
        cli.anvils,
        cli.skip_review,
        cli.keep_branches,
        cli.ci_only,
        cli.review_all,
        cli.retry,
    );

    let result = match cli.command {
        Some(Command::Status) => show_status(),
        Some(Command::Update) => update::self_update().await,
        Some(Command::Resume) => {
            let smith_config = SmithConfig::from_env();
            pipeline::run(None, &smith_config, &pipeline_config).await
        }
        None => {
            let smith_config = SmithConfig::from_env();
            let commission = cli.commission_text();
            pipeline::run(commission.as_deref(), &smith_config, &pipeline_config).await
        }
    };

    if let Err(e) = result {
        eprintln!("\n  \x1b[31m✗\x1b[0m {e}\n");
        std::process::exit(1);
    }
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
    println!(
        "  \x1b[90mBlueprint: {}\x1b[0m",
        if has_bp { "yes" } else { "no" }
    );

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
