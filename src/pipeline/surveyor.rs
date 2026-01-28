use crate::config::{BLUEPRINT, MAX_ITERATE, ORE_FILE};
use crate::error::SlagError;
use crate::flux;
use crate::smith::{self, Smith};
use crate::tui;

/// Phase 1: Analyze the commission (PRD.md) and produce a BLUEPRINT.md
pub async fn run(smith: &dyn Smith) -> Result<(), SlagError> {
    tui::header("SURVEYOR · deep analysis");

    let ore = std::fs::read_to_string(ORE_FILE).map_err(|_| SlagError::NoOre)?;

    let prompt = flux::surveyor_prompt(&ore);
    log_to_file("SURVEY_PROMPT", &prompt);

    let spinner = tui::spinner("surveying...");
    let raw = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        SlagError::SurveyFailed(e.to_string())
    })?;
    spinner.finish_and_clear();

    log_to_file("SURVEY_RAW", &raw);

    // Self-iterate if questions detected
    let raw = smith::self_iterate(smith, raw, MAX_ITERATE).await?;

    std::fs::write(BLUEPRINT, &raw)?;
    tui::status_line("█", tui::PURE, &format!("Blueprint: {BLUEPRINT}"));

    // Show preview
    println!();
    let lines: Vec<&str> = raw.lines().collect();
    for line in lines.iter().take(20) {
        println!("  \x1b[90m{line}\x1b[0m");
    }
    if lines.len() > 20 {
        println!("\n  \x1b[90m... +{} lines\x1b[0m", lines.len() - 20);
    }

    Ok(())
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}
