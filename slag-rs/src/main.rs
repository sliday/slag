#![cfg_attr(test, allow(
    clippy::await_holding_lock,
    clippy::field_reassign_with_default,
    clippy::expect_fun_call,
    clippy::useless_vec,
    clippy::needless_borrows_for_generic_args,
    clippy::manual_range_contains,
    clippy::cloned_ref_to_slice_refs,
    clippy::items_after_test_module,
))]

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
mod replay;
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

    if let Some(provider) = &cli.provider {
        std::env::set_var("SLAG_PROVIDER", provider);
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
    // --no-sandbox = SLAG_SANDBOX=off, read downstream like the rest.
    if cli.no_sandbox {
        std::env::set_var("SLAG_SANDBOX", "off");
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

    // Probe the shell sandbox before any dashboard owns the terminal, so
    // the one "running unconfined" warning stays readable.
    if matches!(cli.command, None | Some(Command::Resume) | Some(Command::Fix(_))) {
        engine::sandbox::preflight();
    }

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
        Some(Command::Resume) => {
            forge(None, cli.anvils, cli.tui, cli.trace.clone(), cli.temper).await
        }
        Some(Command::Rewind { ingot, heat }) => cli::show_rewind(ingot.as_deref(), heat),
        Some(Command::Hooks { action: cli::HooksAction::List }) => cli::show_hooks(),
        Some(Command::Fix(args)) => {
            pipeline::fix::run(args, cli.tui, cli.trace.clone(), cli.temper > 0).await
        }
        Some(Command::Replay { file, speed, tui, at, hold, size, points }) => {
            match size.as_deref().map(replay::parse_size).transpose() {
                Ok(size) => {
                    replay::run(replay::Opts {
                        file,
                        speed,
                        tui: tui || cli.tui,
                        at,
                        hold,
                        size,
                        points,
                    })
                    .await
                }
                Err(e) => Err(error::SlagError::Other(anyhow::anyhow!(e))),
            }
        }
        None => {
            // A lone bare word reaches here because `commission` is a
            // positional var-arg: clap has no subcommand to match, so the
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
                forge(commission.as_deref(), cli.anvils, cli.tui, cli.trace.clone(), cli.temper)
                    .await
            }
        }
    };

    if let Err(e) = result {
        // The assay above already printed the gap in full; the last line
        // only has to say how the run ended.
        let line = match &e {
            error::SlagError::GoalNotMet(_) => "goal not met; the gap is above".to_string(),
            other => other.to_string(),
        };
        eprintln!("\n  {}✗{} {line}\n", fg(tui::WARM), reset());
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
    temper: usize,
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
    let _provider_run = error::ProviderRunGuard::begin();
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

    let result = run_pipeline(commission, &config, anvils, tui_flag, trace, temper).await;

    if let Some(path) = session {
        let _ = std::fs::remove_file(path);
    }
    error::provider_run_failure().map_or(result, Err)
}

/// `slag key [KEY]` — the whole configuration surface. With a key it
/// verifies and saves; without one it reports what slag would use, or
/// onboards when there is nothing to report. Passing the key as an
/// argument leaks it into shell history, so the bare form prompts.
async fn run_key(key: Option<String>) -> Result<(), error::SlagError> {
    let provider = config::active_provider()?;
    println!("  provider: {}", provider.name());
    if let Some(key) = key {
        config::verify_and_store_for_provider(provider, key.trim()).await?;
        // Saving into a shell that exports its own key stores something
        // slag will never read. Warn instead of letting the next run 401.
        if matches!(config::key_status_for_provider(provider), Some((config::KeySource::Env, _))) {
            if provider == config::ProviderPreset::OpenRouter {
                tui::key_shadowed();
            } else {
                println!("  The environment key overrides the stored key.");
            }
        }
        return Ok(());
    }

    let status = config::key_status_for_provider(provider);
    if status.is_none() && !provider.key_optional() && std::io::stdin().is_terminal() {
        config::onboard_provider(provider).await?;
        return Ok(());
    }
    let source = status.as_ref().map(|(src, _)| {
        let label = match src {
            config::KeySource::Env if provider == config::ProviderPreset::Custom
                && std::env::var("CUSTOM_API_KEY").map(|key| key.trim().is_empty()).unwrap_or(true) => "SLAG_API_KEY",
            config::KeySource::Env => provider.key_env().unwrap_or("SLAG_API_KEY"),
            config::KeySource::File => "config file",
        };
        (label, "present".to_string())
    });
    if provider.key_optional() && status.is_none() {
        println!("  key: optional, none set");
    }

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
            let model = provider.default_model().to_string();
            (model.clone(), model.clone(), model.clone(), model)
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

    if provider == config::ProviderPreset::OpenRouter {
        tui::key_panel(source, &models);
    } else {
        if let Some((source, _)) = source {
            println!("  key: present ({source})");
        } else if !provider.key_optional() {
            println!("  key: missing ({})", provider.key_env().unwrap_or("SLAG_API_KEY"));
        }
        for (role, model, _) in models {
            println!("  {role}: {model}");
        }
    }
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
    temper: usize,
) -> Result<(), error::SlagError> {
    connect_mcp().await;

    if !(tui_flag && std::io::stdin().is_terminal()) {
        // Headless: the recording session holds the event channel, and its
        // stream view draws the whole run from it, as the dashboard does.
        // The pipeline goes quiet so nothing prints twice; a replay of the
        // tape then draws exactly what this run drew.
        let tape = render::tape::Session::open(
            Some(render::tape::default_path()),
            Some(Box::new(render::stream::Stream::live())),
        );
        tui::set_presenter(true);
        tui::set_quiet(true);
        let hooks = tape.attach(EngineHooks::default());
        let (hooks, sink) = render::trace::attach(hooks, trace);
        let result =
            pipeline::run(commission, config, anvils, hooks, temper).await;
        if let Some(sink) = sink {
            let _ = sink.await;
        }
        tape.close().await;
        tui::set_quiet(false);
        tui::set_presenter(false);
        return result;
    }

    // The dashboard holds the event channel; the tape only listens.
    let tape = render::tape::Session::open(Some(render::tape::default_path()), None);

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
        let hooks = tape.attach(EngineHooks {
            events: Some(tx),
            steer: Some(steer.clone()),
            cancel: Some(cancel.clone()),
        });
        // Tee before the dashboard: the trace needs the same stream, and
        // the dashboard must not lose an event to get it.
        let (hooks, trace_sink) = render::trace::attach(hooks, trace.clone());

        tui::set_quiet(true);
        let dash = tokio::spawn(dashboard::run(rx, steer.clone(), cancel.clone()));

        let result =
            pipeline::run(commission.as_deref(), config, anvils, hooks.clone(), temper)
                .await;

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

        if error::provider_run_failure().is_some() {
            break Err(error::SlagError::Cancelled);
        }

        match next {
            // A fresh commission re-enters the pipeline from the top:
            // survey, found, forge. The prior run's report is printed
            // first so the screen it replaces is not lost.
            Some(next) => {
                // The alternate screen took this run's ASSAY with it, and
                // the next forge is about to reuse the screen. Print the
                // report now or it is lost between passes.
                if !matches!(result, Err(error::SlagError::Cancelled)) {
                    print_assay();
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
        print_assay();
    }
    tape.close().await;

    result
}

/// The assay in the stream view's words, on the real terminal. The pipeline
/// still records its marks for the tape (`assay::show` with color off goes
/// nowhere while quiet), so a replay sees the same report.
fn print_assay() {
    for line in render::echo::assay_report(tui::width()) {
        println!("{line}");
    }
}

fn show_status() -> Result<(), error::SlagError> {
    tui::show_banner();
    let width = tui::width();
    let label = |name: &str| tui::dim(&format!("{name:<10}"));

    let crucible_path = Path::new(config::CRUCIBLE);
    if !crucible_path.exists() {
        println!();
        println!(
            "  No crucible found here. Start one with {}slag \"describe what to build\"{}",
            fg(tui::HOT),
            reset()
        );
        println!();
        return Ok(());
    }

    let crucible = crucible::Crucible::load(crucible_path)?;
    let counts = crucible.counts();
    println!();

    if let Ok(ore) = std::fs::read_to_string(config::ORE_FILE) {
        if let Some(commission) = dashboard::commission_line(&ore) {
            println!("  {}  {}", label("commission"), tui::clip(&commission, width.saturating_sub(14)));
        }
    }
    let blueprint = match std::fs::read_to_string(config::BLUEPRINT) {
        Ok(doc) => format!("{} · {} lines", config::BLUEPRINT, doc.lines().count()),
        Err(_) => tui::dim("not drafted yet"),
    };
    println!("  {}  {blueprint}", label("blueprint"));
    println!("  {}  {} · {} ingots", label("crucible"), config::CRUCIBLE, counts.total);
    println!("{}", tui::counts_text(&counts, width));
    println!();

    // Every ingot, one row each: this is the one screen that shows them all.
    let shown = 20;
    let id_w = crucible.ingots.iter().map(|i| i.id.chars().count()).max().unwrap_or(2);
    for ingot in crucible.ingots.iter().take(shown) {
        let (glyph, color) = match ingot.status {
            sexp::Status::Forged => ("✓", tui::PURE),
            sexp::Status::Cracked => ("✗", tui::WARM),
            sexp::Status::Molten => ("⚒", tui::HOT),
            sexp::Status::Ore => ("·", tui::COLD),
        };
        let heat = if ingot.heat > 1 { format!("heat {}", ingot.heat) } else { String::new() };
        let room = width.saturating_sub(id_w + 10 + heat.chars().count());
        let title = render::echo::title(&ingot.work);
        let left = format!(
            "    {} {}  {}",
            tui::paint(color, glyph),
            tui::paint(if ingot.status == sexp::Status::Ore { tui::COLD } else { tui::PURE }, &format!("{:<id_w$}", ingot.id)),
            tui::clip(&title, room)
        );
        println!("{}", tui::beside(&left, &tui::paint(color, &heat), width));
    }
    if crucible.ingots.len() > shown {
        println!("    {}", tui::dim(&format!("… {} more in {}", crucible.ingots.len() - shown, config::CRUCIBLE)));
    }

    // What to do next, when there is something to do.
    println!();
    if counts.ore + counts.molten > 0 {
        println!(
            "  {}  {}slag resume{}  {}",
            label("next"),
            fg(tui::HOT),
            reset(),
            tui::dim(&format!("forges the {} left", counts.ore + counts.molten))
        );
    } else if let Some(first) = crucible.ingots.iter().find(|i| i.status == sexp::Status::Cracked) {
        // The same next command the assay gives: the failing proof's own
        // output, not a direction to go looking for it.
        println!(
            "  {}  {}less {}/*_ASSAY_{}_*.log{}  {}",
            label("next"),
            fg(tui::HOT),
            config::LOG_DIR,
            first.id,
            reset(),
            tui::dim("the failing proof's own output")
        );
    } else {
        println!("  {}  {}", label("next"), tui::dim("every ingot forged; commission the next change"));
    }
    println!();
    Ok(())
}
