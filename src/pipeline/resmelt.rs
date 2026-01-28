use std::path::Path;

use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::flux;
use crate::sexp::parser::parse_crucible;
use crate::sexp::Ingot;
use crate::smith::Smith;
use crate::tui;

/// Attempt to re-smelt a cracked ingot.
/// Analyzes failure and either rewrites, splits, or declares impossible.
/// On success, updates the crucible in place (caller must save).
pub async fn resmelt_ingot(
    crucible: &mut Crucible,
    ingot: &Ingot,
    smith: &dyn Smith,
) -> Result<(), SlagError> {
    if ingot.smelt >= 1 {
        println!("    \x1b[31m⚠\x1b[0m already re-smelted, truly cracked");
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    println!(
        "\n  \x1b[38;5;208m♻\x1b[0m \x1b[1;37mRE-SMELTING [{}]\x1b[0m — analyzing failure...",
        ingot.id
    );

    // Gather failure logs
    let failure_logs = gather_failure_logs(&ingot.id);

    let prompt = flux::prepare_resmelt_flux(ingot, &failure_logs);
    log_to_file(&format!("RESMELT_{}", ingot.id), &prompt);

    let spinner = tui::spinner("re-smelting...");
    let response = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        println!("    \x1b[31m✗\x1b[0m smelter failed");
        SlagError::SmithFailed(e.to_string())
    })?;
    spinner.finish_and_clear();

    log_to_file(&format!("RESMELT_RESULT_{}", ingot.id), &response);

    // Parse response
    if response.contains("IMPOSSIBLE:") {
        let reason = response
            .lines()
            .find(|l| l.starts_with("IMPOSSIBLE:"))
            .map(|l| l.strip_prefix("IMPOSSIBLE:").unwrap().trim())
            .unwrap_or("unknown");
        println!(
            "    \x1b[31m✗\x1b[0m impossible: {}",
            &reason[..reason.len().min(60)]
        );
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    // Try to extract ingot lines from response
    let new_ingots = parse_crucible(&response);
    if new_ingots.is_empty() {
        println!("    \x1b[31m✗\x1b[0m could not parse smelter output");
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    if new_ingots.len() == 1 {
        // Rewrite
        println!(
            "    \x1b[38;5;220m♻\x1b[0m rewritten: {}",
            tui::truncate(&new_ingots[0].work, 50)
        );
    } else {
        // Split
        println!(
            "    \x1b[38;5;220m♻\x1b[0m split into {} sub-ingots",
            new_ingots.len()
        );
    }

    // Replace in crucible — the old ingot becomes the new one(s)
    // Mark the old ingot position with the replacement(s)
    crucible.replace(&ingot.id, new_ingots);

    Ok(())
}

fn gather_failure_logs(id: &str) -> String {
    let log_dir = Path::new(crate::config::LOG_DIR);
    let mut logs = String::new();

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(id) {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    let lines: Vec<&str> = content.lines().collect();
                    let tail: Vec<&str> = lines.iter().rev().take(50).rev().cloned().collect();
                    logs.push_str(&format!("--- {name} ---\n"));
                    logs.push_str(&tail.join("\n"));
                    logs.push('\n');
                }
            }
        }
    }

    if logs.is_empty() {
        "No failure logs found".into()
    } else {
        logs
    }
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}
