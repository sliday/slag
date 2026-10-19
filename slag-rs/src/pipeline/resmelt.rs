use std::path::Path;

use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::flux;
use crate::sexp::parser::parse_crucible;
use crate::sexp::{Ingot, Status};
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
    let quiet = tui::is_quiet();
    if ingot.smelt >= 1 {
        if !quiet {
            println!(
                "    {}⚠{} already re-smelted, truly cracked",
                super::fg(tui::WARM),
                super::reset()
            );
        }
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    if !quiet {
        println!(
            "\n  {}♻{} {}{}RE-SMELTING [{}]{} — analyzing failure...",
            super::fg(tui::HOT),
            super::reset(),
            super::bold(),
            super::fg(tui::PURE),
            ingot.id,
            super::reset()
        );
    }

    // Gather failure logs
    let failure_logs = gather_failure_logs(&ingot.id);

    let prompt = flux::prepare_resmelt_flux(ingot, &failure_logs);
    log_to_file(&format!("RESMELT_{}", ingot.id), &prompt);

    let spinner = tui::spinner("re-smelting...");
    let response = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        if !quiet {
            println!("    {}✗{} smelter failed", super::fg(tui::WARM), super::reset());
        }
        if e.is_deterministic_provider() { e } else { SlagError::SmithFailed(e.to_string()) }
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
        if !quiet {
            println!(
                "    {}✗{} impossible: {}",
                super::fg(tui::WARM),
                super::reset(),
                &reason[..reason.len().min(60)]
            );
        }
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    // Try to extract ingot lines from response
    let mut new_ingots = parse_crucible(&response);
    if new_ingots.is_empty() {
        if !quiet {
            println!(
                "    {}✗{} could not parse smelter output",
                super::fg(tui::WARM),
                super::reset()
            );
        }
        return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
    }

    let mut ids: std::collections::HashSet<_> = crucible.ingots
        .iter()
        .filter(|i| i.id != ingot.id)
        .map(|i| i.id.clone())
        .collect();
    for replacement in &mut new_ingots {
        if !ids.insert(replacement.id.clone()) {
            return Err(SlagError::IngotCracked(ingot.id.clone(), ingot.max));
        }
        replacement.status = Status::Ore;
        replacement.heat = 0;
        replacement.smelt = ingot.smelt + 1;
    }

    if !quiet {
        if new_ingots.len() == 1 {
            // Rewrite
            println!(
                "    {}♻{} rewritten: {}",
                super::fg(tui::BRIGHT),
                super::reset(),
                tui::truncate(&new_ingots[0].work, 50)
            );
        } else {
            // Split
            println!(
                "    {}♻{} split into {} sub-ingots",
                super::fg(tui::BRIGHT),
                super::reset(),
                new_ingots.len()
            );
        }
    }

    // Replace in crucible — the old ingot becomes the new one(s)
    // Mark the old ingot position with the replacement(s)
    crucible.replace(&ingot.id, new_ingots);

    Ok(())
}

fn gather_failure_logs(id: &str) -> String {
    let log_dir = Path::new(crate::config::project_path(crate::config::LOG_DIR));
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
    let path = format!("{}/{ts}_{label}.log", crate::config::project_path(crate::config::LOG_DIR));
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::Status;
    use crate::smith::mock::MockSmith;

    #[tokio::test]
    async fn resmelt_owns_status_heat_and_smelt_for_rewrites_and_splits() {
        let dir = tempfile::tempdir().unwrap();
        for response in [
            "(ingot :id \"i1\" :status molten :heat 99 :smelt 0)",
            "(ingot :id \"i1a\" :status forged :heat 4 :smelt 0)\n\
             (ingot :id \"i1b\" :status cracked :heat 5 :smelt 9)",
        ] {
            let ingot = parse_crucible("(ingot :id \"i1\" :status molten :heat 5)").remove(0);
            let mut crucible = Crucible::new(&dir.path().join("PLAN.md"), vec![ingot.clone()]);
            let smith = MockSmith::fixed(response);
            resmelt_ingot(&mut crucible, &ingot, &smith).await.unwrap();
            for replacement in crucible.ingots.clone() {
                assert_eq!(replacement.status, Status::Ore);
                assert_eq!(replacement.heat, 0);
                assert_eq!(replacement.smelt, 1);
                resmelt_ingot(&mut crucible, &replacement, &smith).await
                    .expect_err("a replacement gets one re-smelt");
            }
            assert_eq!(smith.call_count(), 1);
        }
    }

    #[tokio::test]
    async fn resmelt_rejects_duplicate_or_colliding_replacement_ids() {
        let dir = tempfile::tempdir().unwrap();
        for response in [
            "(ingot :id \"i1\")\n(ingot :id \"i1\")",
            "(ingot :id \"i2\")",
        ] {
            let ingots = parse_crucible("(ingot :id \"i1\" :status molten :heat 5)\n\
                (ingot :id \"i2\" :status forged)");
            let original = ingots[0].clone();
            let mut crucible = Crucible::new(&dir.path().join("PLAN.md"), ingots);
            resmelt_ingot(&mut crucible, &original, &MockSmith::fixed(response)).await
                .expect_err("re-smelt cannot alias another ingot");
            assert_eq!(crucible.ingots.len(), 2);
            assert_eq!(crucible.get("i1").unwrap().heat, 5);
            assert_eq!(crucible.get("i2").unwrap().status, Status::Forged);
        }
    }
}
