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
        Ok(true)
    } else {
        println!("\n  \x1b[31m✗\x1b[0m No recoverable ingots");

        // Ask user if they want to force retry all cracked ingots
        if ask_force_retry(counts.cracked) {
            let mut crucible = Crucible::load(crucible_path)?;
            for id in &cracked_ids {
                if let Some(ingot) = crucible.get_mut(id) {
                    ingot.status = Status::Ore;
                    ingot.heat = 0;
                    // Keep smelt count to track attempts
                }
            }
            crucible.save()?;
            println!(
                "\n  \x1b[38;5;220m⚒\x1b[0m Force retry: {} ingots reset to ore",
                counts.cracked
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Detect the failure pattern for a cracked ingot by reading logs
fn detect_failure_pattern(ingot: &Ingot) -> FailurePattern {
    let log_dir = Path::new(crate::config::LOG_DIR);

    // Collect all matching log files (sorted by time, newest first)
    let mut matching_logs: Vec<_> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Match logs for this ingot (ASSAY, STRIKE, FLUX)
            if name.contains(&ingot.id) {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    matching_logs.push((name, content));
                }
            }
        }
    }

    // Analyze all logs for this ingot
    for (_name, content) in &matching_logs {
        // Check for dependency/file issues
        if content.contains("No such file or directory")
            || content.contains("file not found")
            || content.contains("does not exist")
            || content.contains("ENOENT")
            || content.contains("cannot open")
        {
            return FailurePattern::MissingDependency {
                file: extract_missing_file(content).unwrap_or_else(|| "unknown".to_string()),
            };
        }

        // Check for JSON/parsing issues (common with data tasks)
        if content.contains("parse error")
            || content.contains("invalid JSON")
            || content.contains("jq:")
            || content.contains("SyntaxError")
        {
            return FailurePattern::ProofMismatch;
        }

        // Check for protocol failures
        if content.contains("NO CMD:") || content.contains("missing \"CMD:\"") {
            return FailurePattern::ProtocolFailure;
        }

        // Check for proof failures
        if content.contains("proof failed") || content.contains("non-zero exit") {
            return FailurePattern::ProofMismatch;
        }
    }

    // No logs found or no patterns matched - try to infer from ingot properties
    // If proof involves file operations and ingot is parallel, it may need sequencing
    let proof = ingot.proof.to_lowercase();
    if ingot.solo
        && (proof.contains("test -f")
            || proof.contains("test -d")
            || proof.contains("cat ")
            || proof.contains("jq "))
    {
        // Likely depends on a file that should be created by another ingot
        return FailurePattern::MissingDependency {
            file: extract_file_from_proof(&ingot.proof).unwrap_or_else(|| "unknown".to_string()),
        };
    }

    FailurePattern::Unknown
}

/// Extract file path from a proof command
fn extract_file_from_proof(proof: &str) -> Option<String> {
    // Look for patterns like "test -f FILE" or "jq . FILE" or "cat FILE"
    let parts: Vec<&str> = proof.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if (*part == "-f" || *part == "-d" || *part == "." || *part == "-e") && i + 1 < parts.len()
        {
            let file = parts[i + 1];
            if !file.starts_with('-') && !file.starts_with('|') {
                return Some(file.to_string());
            }
        }
        // Last argument is often the file
        if i == parts.len() - 1
            && !part.starts_with('-')
            && !part.starts_with('|')
            && part.contains('/')
        {
            return Some(part.to_string());
        }
    }
    None
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
            // Unknown failure - be aggressive with retries
            // Only skip if truly exhausted (smelt >= 3 means reconsidered twice)
            if ingot.smelt >= 3 {
                AnalysisAction::Skip
            } else if ingot.smelt >= 2 {
                // Already reconsidered once, regenerate with fresh approach
                AnalysisAction::Regenerate
            } else {
                AnalysisAction::Retry
            }
        }
    }
}

/// Ask user if they want to force retry all cracked ingots
fn ask_force_retry(cracked_count: usize) -> bool {
    use std::io::{self, Write};

    print!(
        "\n  \x1b[38;5;220m?\x1b[0m Force retry {} cracked ingots? [y/N] ",
        cracked_count
    );
    io::stdout().flush().unwrap();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let trimmed = input.trim().to_lowercase();
        return trimmed == "y" || trimmed == "yes";
    }
    false
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
