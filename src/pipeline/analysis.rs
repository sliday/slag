use std::path::Path;

use crate::config::{SmithConfig, CRUCIBLE};
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::flux;
use crate::sexp::{Ingot, Status};
use crate::smith::Smith;
use crate::tui;

/// Failure pattern detected during analysis
#[derive(Debug, Clone)]
pub enum FailurePattern {
    /// File/directory not found - dependency ordering issue
    MissingDependency { file: String },
    /// CMD: line missing from smith output - protocol failure
    ProtocolFailure,
    /// Proof command failed - work/proof mismatch
    ProofMismatch,
    /// Unknown failure
    Unknown,
}

/// Analysis result for a cracked ingot
#[derive(Debug)]
pub struct CrackedAnalysis {
    pub id: String,
    pub pattern: FailurePattern,
    pub recommendation: AnalysisAction,
}

/// Recommended action from analysis
#[derive(Debug, Clone)]
pub enum AnalysisAction {
    /// Reset to ore and retry (simple retry)
    Retry,
    /// Mark as sequential (was parallel but has dependencies)
    MakeSequential,
    /// Needs founder to regenerate
    Regenerate,
    /// Truly impossible, skip
    Skip,
}

/// Analyze cracked ingots and prepare for retry
pub async fn analyze_and_prepare(
    smith: &dyn Smith,
    _config: &SmithConfig,
    cycle: usize,
) -> Result<bool, SlagError> {
    let crucible_path = Path::new(CRUCIBLE);
    let mut crucible = Crucible::load(crucible_path)?;
    let counts = crucible.counts();

    if counts.cracked == 0 {
        return Ok(false); // Nothing to analyze
    }

    tui::header(&format!("ANALYSIS · retry cycle {}", cycle));

    println!(
        "  \x1b[38;5;208m⚗\x1b[0m Analyzing {} cracked ingots...",
        counts.cracked
    );

    // Gather failure logs and analyze each cracked ingot
    let cracked_ids: Vec<String> = crucible
        .ingots
        .iter()
        .filter(|i| i.status == Status::Cracked)
        .map(|i| i.id.clone())
        .collect();

    let mut analyses: Vec<CrackedAnalysis> = Vec::new();

    for id in &cracked_ids {
        if let Some(ingot) = crucible.get(id) {
            let pattern = detect_failure_pattern(ingot);
            let recommendation = recommend_action(&pattern, ingot);
            println!(
                "    \x1b[90m[{}]\x1b[0m {:?} → {:?}",
                id, pattern, recommendation
            );
            analyses.push(CrackedAnalysis {
                id: id.clone(),
                pattern,
                recommendation,
            });
        }
    }

    // Check if we should regenerate via founder or just retry
    let needs_regenerate = analyses
        .iter()
        .any(|a| matches!(a.recommendation, AnalysisAction::Regenerate));

    if needs_regenerate {
        // Use AI to regenerate the failed portion
        println!(
            "\n  \x1b[38;5;220m♻\x1b[0m Regenerating {} cracked ingots via founder...",
            cracked_ids.len()
        );
        regenerate_cracked(smith, &mut crucible, &cracked_ids).await?;
    } else {
        // Apply fixes and reset to ore
        for analysis in &analyses {
            match analysis.recommendation {
                AnalysisAction::Retry => {
                    // Reset to ore, clear heat and smelt
                    if let Some(ingot) = crucible.get_mut(&analysis.id) {
                        ingot.status = Status::Ore;
                        ingot.heat = 0;
                        ingot.smelt = 0;
                    }
                    println!("    \x1b[38;5;220m↺\x1b[0m [{}] reset to ore", analysis.id);
                }
                AnalysisAction::MakeSequential => {
                    // Mark as sequential and reset
                    if let Some(ingot) = crucible.get_mut(&analysis.id) {
                        ingot.status = Status::Ore;
                        ingot.heat = 0;
                        ingot.smelt = 0;
                        ingot.solo = false; // Make sequential
                    }
                    println!(
                        "    \x1b[38;5;220m↺\x1b[0m [{}] reset to ore (now sequential)",
                        analysis.id
                    );
                }
                AnalysisAction::Skip => {
                    println!(
                        "    \x1b[31m✗\x1b[0m [{}] skipped (truly impossible)",
                        analysis.id
                    );
                }
                AnalysisAction::Regenerate => {
                    // Already handled above
                }
            }
        }
    }

    crucible.save()?;

    // Check if we have any ore to forge
    let new_counts = crucible.counts();
    let has_work = new_counts.ore > 0;

    if has_work {
        println!(
            "\n  \x1b[38;5;220m⚒\x1b[0m Ready to retry: {} ingots reset to ore",
            new_counts.ore
        );
    } else {
        println!("\n  \x1b[31m✗\x1b[0m No recoverable ingots");
    }

    Ok(has_work)
}

/// Detect the failure pattern for a cracked ingot by reading logs
fn detect_failure_pattern(ingot: &Ingot) -> FailurePattern {
    let log_dir = Path::new(crate::config::LOG_DIR);

    // Look for failure logs
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(&ingot.id) && (name.contains("ASSAY") || name.contains("STRIKE")) {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    // Check for common patterns
                    if content.contains("No such file or directory")
                        || content.contains("file not found")
                        || content.contains("does not exist")
                    {
                        // Try to extract the file name
                        return FailurePattern::MissingDependency {
                            file: extract_missing_file(&content)
                                .unwrap_or_else(|| "unknown".to_string()),
                        };
                    }
                    if content.contains("NO CMD:") || content.contains("missing \"CMD:\"") {
                        return FailurePattern::ProtocolFailure;
                    }
                    if content.contains("proof failed") || content.contains("exit 1") {
                        return FailurePattern::ProofMismatch;
                    }
                }
            }
        }
    }

    FailurePattern::Unknown
}

/// Extract the missing file name from error output
fn extract_missing_file(content: &str) -> Option<String> {
    // Look for patterns like "test -f FILE" or "No such file: FILE"
    for line in content.lines() {
        if line.contains("No such file") {
            if let Some(pos) = line.rfind(':') {
                return Some(line[pos + 1..].trim().to_string());
            }
        }
        if line.contains("test -f") || line.contains("test -d") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                return Some(parts[2].to_string());
            }
        }
    }
    None
}

/// Recommend action based on failure pattern
fn recommend_action(pattern: &FailurePattern, ingot: &Ingot) -> AnalysisAction {
    match pattern {
        FailurePattern::MissingDependency { .. } => {
            // If it was parallel, make it sequential
            if ingot.solo {
                AnalysisAction::MakeSequential
            } else {
                // Already sequential, needs regeneration
                AnalysisAction::Regenerate
            }
        }
        FailurePattern::ProtocolFailure => {
            // Smith didn't follow protocol - retry with reset
            if ingot.smelt >= 2 {
                AnalysisAction::Regenerate
            } else {
                AnalysisAction::Retry
            }
        }
        FailurePattern::ProofMismatch => {
            // Proof/work mismatch - needs regeneration if already re-smelted
            if ingot.smelt >= 1 {
                AnalysisAction::Regenerate
            } else {
                AnalysisAction::Retry
            }
        }
        FailurePattern::Unknown => {
            // Unknown - try retry first
            if ingot.smelt >= 2 {
                AnalysisAction::Skip
            } else {
                AnalysisAction::Retry
            }
        }
    }
}

/// Regenerate cracked ingots using the founder
async fn regenerate_cracked(
    smith: &dyn Smith,
    crucible: &mut Crucible,
    cracked_ids: &[String],
) -> Result<(), SlagError> {
    // Build a prompt describing what needs to be regenerated
    let cracked_descriptions: Vec<String> = cracked_ids
        .iter()
        .filter_map(|id| crucible.get(id))
        .map(|i| format!("[{}] {}", i.id, i.work))
        .collect();

    let prompt = flux::regenerate_prompt(&cracked_descriptions.join("\n"));

    let spinner = tui::spinner("regenerating...");
    let response = smith.invoke(&prompt).await.map_err(|e| {
        spinner.finish_and_clear();
        SlagError::FounderFailed(e.to_string())
    })?;
    spinner.finish_and_clear();

    // Parse new ingots from response
    let new_ingots = crate::crucible::parse_ingot_lines(&response);

    if new_ingots.is_empty() {
        println!("    \x1b[31m✗\x1b[0m could not regenerate ingots");
        // Fall back to simple reset
        for id in cracked_ids {
            if let Some(ingot) = crucible.get_mut(id) {
                ingot.status = Status::Ore;
                ingot.heat = 0;
                ingot.smelt = 0;
                ingot.solo = false; // Make sequential to be safe
            }
        }
        return Ok(());
    }

    println!(
        "    \x1b[38;5;220m♻\x1b[0m generated {} replacement ingots",
        new_ingots.len()
    );

    // Remove all cracked ingots
    for id in cracked_ids {
        crucible.ingots.retain(|i| i.id != *id);
    }

    // Add new ingots (they're already marked as ore with smelt=0)
    for ingot in new_ingots {
        println!(
            "    \x1b[38;5;220m+\x1b[0m [{}] {}",
            ingot.id,
            tui::truncate(&ingot.work, 50)
        );
        crucible.ingots.push(ingot);
    }

    Ok(())
}
