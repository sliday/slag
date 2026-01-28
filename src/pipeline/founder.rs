use crate::config::{BLUEPRINT, CRUCIBLE, MAX_ITERATE, ORE_FILE};
use crate::crucible::{self, Crucible};
use crate::error::SlagError;
use crate::flux;
use crate::smith::{self, Smith};
use crate::tui;

/// Phase 2: Read blueprint and produce S-expression ingots in PLAN.md
pub async fn run(smith: &dyn Smith) -> Result<(), SlagError> {
    tui::header("FOUNDER · casting mold");

    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "No blueprint".into());

    let prompt = flux::founder_prompt(&ore, &blueprint);
    log_to_file("FOUNDER_PROMPT", &prompt);

    let spinner = tui::spinner("casting...");
    let raw = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        SlagError::FounderFailed(e.to_string())
    })?;
    spinner.finish_and_clear();

    log_to_file("FOUNDER_RAW", &raw);

    // Self-iterate if questions
    let raw = smith::self_iterate(smith, raw, MAX_ITERATE).await?;

    let ingots = crucible::parse_ingot_lines(&raw);
    if ingots.is_empty() {
        return Err(SlagError::NoIngots);
    }

    // Create crucible
    let crucible_path = std::path::PathBuf::from(CRUCIBLE);
    let crucible = Crucible::new(&crucible_path, ingots.clone());
    crucible.save()?;

    // Stats
    let count = ingots.len();
    let simple = ingots.iter().filter(|i| !i.is_complex()).count();
    let complex = ingots.iter().filter(|i| i.is_complex()).count();
    let web = ingots.iter().filter(|i| i.is_web()).count();

    tui::status_line(
        "█",
        tui::PURE,
        &format!("Mold: {count} ingots ({simple} simple, {complex} complex, {web} web)"),
    );

    // Show table
    println!();
    println!("  \x1b[90m{:<5} {:<10} WORK\x1b[0m", "ID", "STATUS");
    for (i, ingot) in ingots.iter().enumerate() {
        if i >= 10 {
            break;
        }
        let status_display = match ingot.status {
            crate::sexp::Status::Ore => "\x1b[90m🧱 ore\x1b[0m",
            crate::sexp::Status::Molten => "\x1b[38;5;208m🔥 hot\x1b[0m",
            crate::sexp::Status::Forged => "✅ done",
            crate::sexp::Status::Cracked => "\x1b[31m❌ fail\x1b[0m",
        };
        println!(
            "  \x1b[38;5;208m{:<5}\x1b[0m {:<10} {}",
            ingot.id,
            status_display,
            tui::truncate(&ingot.work, 55),
        );
    }
    if count > 10 {
        println!("  \x1b[90m+{} more\x1b[0m", count - 10);
    }

    Ok(())
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}
