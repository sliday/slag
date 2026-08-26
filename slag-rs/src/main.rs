mod anvil;
mod cli;
mod config;
mod crucible;
mod dashboard;
mod engine;
mod error;
mod flux;
mod insights;
mod migrations;
mod pipeline;
mod progress;
mod proof;
mod render;
mod sexp;
mod shutdown;
mod smith;
mod steer_history;
mod tui;
mod update;

use std::io::IsTerminal;
use std::path::Path;

use clap::Parser;

use cli::{Cli, Command};
use config::EngineConfig;
use pipeline::{fg, reset};
use smith::EngineHooks;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Advertised isolation that silently does nothing corrupts trust (and
    // repos): until wired, say so loudly instead of ignoring the flag.
    if cli.worktree {
        eprintln!(
            "  {}⚠{} --worktree is not implemented yet; \
             all ingots run in the shared checkout",
            fg(tui::BRIGHT),
            reset()
        );
    }

    // Model flags override env; EngineConfig::load reads env downstream.
    // --auto first, so explicit model flags win over it.
    if cli.auto {
        for var in ["SLAG_MODEL_BASE", "SLAG_MODEL_PLAN", "SLAG_MODEL_ALT", "SLAG_MODEL_JUDGE"] {
            std::env::set_var(var, config::AUTO_MODEL);
        }
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
    // --duel = SLAG_DUEL=on: every solo ingot forges with at least two
    // casts. EngineConfig::load reads the env downstream like the rest.
    if cli.duel {
        std::env::set_var("SLAG_DUEL", "on");
    }

    // Cleanup routing, installed before anything can register a chore:
    // a panic mid-draw and a shell Ctrl-C both drain the same registry,
    // so neither leaves the terminal in raw mode or drops buffered
    // events. Registrations come later, from whoever claims a resource.
    shutdown::install_panic_hook();
    shutdown::install_signal_handler();

    // Idempotent fixups (deprecated model slugs, old crucible headers)
    // run before any command touches those files.
    migrations::run();

    // `status`, `update` and `key` inspect or repair a broken setup, so
    // none of them may demand the very key the user came here to fix.
    let result = match cli.command {
        Some(Command::Status { json: true }) => {
            cli::status_json().map(|line| println!("{line}"))
        }
        Some(Command::Status { json: false }) => show_status(),
        Some(Command::Runs) => cli::show_runs(),
        Some(Command::Cost) => cli::show_cost().await,
        Some(Command::Insights { refresh }) => insights::run(Path::new("."), refresh),
        Some(Command::Ps) => cli::show_ps(),
        Some(Command::Update) => update::self_update().await,
        Some(Command::Key { key }) => run_key(key).await,
        Some(Command::Resume) => forge(None, cli.anvils, cli.tui, cli.trace.clone()).await,
        Some(Command::Rewind { ingot, heat }) => cli::show_rewind(ingot.as_deref(), heat),
        Some(Command::Hooks { action: cli::HooksAction::List }) => cli::show_hooks(),
        None => {
            // A lone bare word reaches here because `commission` is a
            // trailing var-arg: clap has no subcommand to match, so the
            // typo becomes a project brief. Stop before that costs an
            // addendum and a planning pass.
            if let Some(word) = cli.suspect_verb().filter(|_| !cli.force) {
                tui::show_banner();
                println!(
                    "  {}! `slag {word}` is not a subcommand{}",
                    fg(tui::WARM),
                    reset()
                );
                if let Some(near) = cli::nearest_subcommand(word) {
                    println!("    did you mean `slag {near}`?");
                }
                println!("    subcommands: {}", cli::SUBCOMMANDS.join(", "));
                println!(
                    "    to commission a project by this name: {}slag --force {word}{}",
                    fg(tui::COLD),
                    reset()
                );
                Ok(())
            } else {
                let commission = cli.commission_text();
                forge(commission.as_deref(), cli.anvils, cli.tui, cli.trace.clone()).await
            }
        }
    };

    if let Err(e) = result {
        eprintln!("\n  {}✗{} {e}\n", fg(tui::WARM), reset());
        resume_hint();
        std::process::exit(1);
    }
}

/// After an interrupted exit (dashboard Ctrl-C, provider failure, budget
/// stop) with work still in the crucible, tell the operator the run is
/// resumable. Printed on the error path only, after the ratatui teardown,
/// so it lands on the real terminal where copy-paste works.
fn resume_hint() {
    let path = Path::new(config::CRUCIBLE);
    if !path.exists() {
        return;
    }
    let Ok(crucible) = crucible::Crucible::load(path) else {
        return;
    };
    let counts = crucible.counts();
    if counts.ore + counts.molten == 0 {
        return;
    }
    let molten = if counts.molten > 0 {
        format!(", {} molten (reset to ore on resume)", counts.molten)
    } else {
        String::new()
    };
    eprintln!(
        "  {} ore{molten} remaining — resume with: {}slag resume{}\n",
        counts.ore,
        fg(tui::HOT),
        reset()
    );
}

/// Resolve the key (onboarding on first run) and forge. The key gate
/// runs before any project file is touched: a run that cannot call a
/// model should not leave a half-lit furnace behind.
async fn forge(
    commission: Option<&str>,
    anvils: usize,
    tui_flag: bool,
    trace: Option<std::path::PathBuf>,
) -> Result<(), error::SlagError> {
    // Two forges rewriting one crucible corrupt each other; refuse the
    // second before any project file is touched. Dead pids were already
    // pruned by the liveness check, so only a genuinely live forge blocks.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Some(dir) = cli::sessions_dir() {
        if let Some(other) = cli::conflict_in(&dir, &cwd) {
            return Err(error::SlagError::Config(format!(
                "another forge (pid {}, started {}) is already lit on this crucible \
                 directory — wait for it, or kill it and rerun",
                other.pid, other.started_at
            )));
        }
    }

    let config = EngineConfig::resolve().await?;
    // Only a forge writes logs. `slag key` run from $HOME should not leave
    // an empty ~/logs behind.
    let _ = std::fs::create_dir_all(config::LOG_DIR);

    // Register in the PID registry so `slag ps` sees this forge; the
    // entry is removed on clean exit, and a crashed forge's stale entry
    // is pruned by the next liveness check.
    let run_id = format!("run-{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
    let session = cli::sessions_dir()
        .and_then(|dir| cli::register_session_in(&dir, &run_id, "forge"));

    // Only a forge can strand Molten ingots, so the rescue registers here
    // rather than in main: `slag status` has nothing to hand back.
    shutdown::register_crucible_rescue();
    if let Some(path) = session.clone() {
        // Drop the PID entry on an abrupt exit too, or `slag ps` reports
        // a ghost forge until the next liveness sweep prunes it.
        shutdown::register(move || {
            let _ = std::fs::remove_file(&path);
        });
    }

    let result = run_pipeline(commission, &config, anvils, tui_flag, trace).await;

    if let Some(path) = session {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// `slag key [KEY]` — the whole configuration surface. With a key it
/// verifies and saves; without one it reports what slag would use, or
/// onboards when there is nothing to report. Passing the key as an
/// argument leaks it into shell history, so the bare form prompts.
async fn run_key(key: Option<String>) -> Result<(), error::SlagError> {
    if let Some(key) = key {
        config::verify_and_store(key.trim()).await?;
        // Saving into a shell that exports its own key stores something
        // slag will never read. Warn instead of letting the next run 401.
        if matches!(config::key_status(), Some((config::KeySource::Env, _))) {
            tui::key_shadowed();
        }
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
    let cfg = EngineConfig::load();
    let (base, plan, alt, judge) = match &cfg {
        Some(cfg) => (
            cfg.model_base.clone(),
            cfg.model_plan.clone(),
            cfg.model_alt.clone(),
            cfg.model_judge.clone(),
        ),
        None => {
            let auto = config::AUTO_MODEL.to_string();
            (auto.clone(), auto.clone(), auto.clone(), auto)
        }
    };

    // The alt and judge roles only run when a duel does. Matched models
    // no longer idle them: the direction prompts carry cast diversity,
    // so only SLAG_DUEL=off parks these roles now.
    let duels = cfg.as_ref().is_some_and(|c| c.duel_qualifies(5));
    let idle = (!duels).then_some("idle — SLAG_DUEL=off");
    let duel_state = match cfg.as_ref().map(|c| c.duel) {
        Some(config::DuelMode::On) => "on — solo ingots forge with 2-3 casts",
        Some(config::DuelMode::Off) => "off — every ingot forges with a single cast",
        _ => "auto — 1-3 casts by grade/work; direction prompts carry diversity",
    };
    let models = [
        ("work", base.as_str(), None),
        ("plan", plan.as_str(), None),
        ("alt", alt.as_str(), idle),
        ("judge", judge.as_str(), idle),
        ("duel", duel_state, None),
    ];

    tui::key_panel(source, &models);
    Ok(())
}

/// Spawn the `[mcp]` servers and report what came up. Runs before the
/// dashboard takes the terminal so warnings stay readable. Servers that
/// fail are named and skipped; the forge proceeds on the natives alone.
async fn connect_mcp() {
    for warning in engine::mcp::connect_configured().await {
        tui::status_line("⚠", tui::WARM, &warning);
    }
    let Some(registry) = engine::mcp::registry().filter(|r| !r.is_empty()) else {
        return;
    };
    let (servers, tools) = registry.counts();
    tui::status_line(
        "⚙",
        tui::COLD,
        &format!(
            "mcp: {servers} server(s) up ({}), {tools} tool(s)",
            registry.server_names().join(", ")
        ),
    );
}

/// Run the pipeline, optionally under the full-screen dashboard.
/// `--tui` needs a real terminal on stdin for the key reader; headless
/// runs (CI, pipes) silently keep the stream-mode display.
async fn run_pipeline(
    commission: Option<&str>,
    config: &EngineConfig,
    anvils: usize,
    tui_flag: bool,
    trace: Option<std::path::PathBuf>,
) -> Result<(), error::SlagError> {
    connect_mcp().await;

    if !(tui_flag && std::io::stdin().is_terminal()) {
        // `--trace` without `--tui` is the useful headless case: no
        // dashboard holds the channel, so the trace sink takes it.
        let (hooks, sink) = render::trace::attach(EngineHooks::default(), trace);
        let result = pipeline::run(commission, config, anvils, hooks).await;
        if let Some(sink) = sink {
            let _ = sink.await;
        }
        return result;
    }

    // One pass per commission. A finished forge is not a full stop: the
    // dashboard hands back whatever the operator typed at the end, and
    // that becomes the next commission. The steer queue and cancel flag
    // outlive a pass; the event channel does not, because closing it is
    // how the dashboard learns a forge is over.
    let steer = engine::SteerQueue::default();
    let cancel = engine::CancelFlag::default();
    let mut commission: Option<String> = commission.map(str::to_string);
    let result = loop {
        let (tx, rx) = engine::events::channel();
        let hooks = EngineHooks {
            events: Some(tx),
            steer: Some(steer.clone()),
            cancel: Some(cancel.clone()),
        };
        // Tee before the dashboard: the trace needs the same stream, and
        // the dashboard must not lose an event to get it.
        let (hooks, trace_sink) = render::trace::attach(hooks, trace.clone());

        tui::set_quiet(true);
        let dash = tokio::spawn(dashboard::run(rx, steer.clone(), cancel.clone()));

        let result =
            pipeline::run(commission.as_deref(), config, anvils, hooks.clone()).await;

        // Surface pipeline-level failures (crucible parse, ForgeFailed, IO)
        // in the dashboard feed; otherwise the run ends with no visible
        // signal and the app looks hung.
        if let Err(e) = &result {
            engine::emit(
                &hooks.events,
                engine::EngineEvent::Error { message: format!("pipeline stopped: {e}") },
            );
        }

        // Drop every EventTx so the dashboard drains its channel; it stays
        // up for review until the user detaches (q/Esc) or commissions
        // another forge, then restores the terminal and un-quiets the
        // stream tui itself.
        drop(hooks);
        let next = dash.await.ok().and_then(|r| r.ok()).flatten();
        // The tee closes with the hooks; wait for the trace's closing
        // bracket before returning, or a fast exit truncates the file.
        if let Some(sink) = trace_sink {
            let _ = sink.await;
        }
        tui::set_quiet(false);

        match next {
            // A fresh commission re-enters the pipeline from the top:
            // survey, found, forge. The prior run's report is printed
            // first so the screen it replaces is not lost.
            Some(next) => {
                // The alternate screen took this run's ASSAY with it, and
                // the next forge is about to reuse the screen. Print the
                // report now or it is lost between passes.
                if !matches!(result, Err(error::SlagError::Cancelled)) {
                    let _ = pipeline::assay::show();
                }
                commission = Some(next);
            }
            None => break result,
        }
    };

    // The alternate screen took the in-dashboard ASSAY output with it;
    // reprint the final report on the real terminal. A cracked run needs
    // it most, so only a cancel (nothing to report) skips it.
    if !matches!(result, Err(error::SlagError::Cancelled)) {
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
            "\n  {}Commission:{} {}",
            fg(tui::HOT),
            reset(),
            tui::truncate(commission, 50)
        );
    }

    let has_bp = Path::new(config::BLUEPRINT).exists();
    println!(
        "  {}Blueprint: {}{}",
        fg(tui::COLD),
        if has_bp { "yes" } else { "no" },
        reset()
    );

    print!("  ");
    tui::ingot_status_line(&counts);
    println!();
    tui::temper_bar(&counts);

    if counts.cracked > 0 {
        println!("\n  {}Cracked:{}", fg(tui::WARM), reset());
        for ingot in &crucible.ingots {
            if ingot.status == sexp::Status::Cracked {
                println!("    {}✗{} [{}] {}", fg(tui::WARM), reset(), ingot.id, ingot.work);
            }
        }
    }

    println!();
    Ok(())
}
