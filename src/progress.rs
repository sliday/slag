use std::io::Write;

use crate::config::LEDGER;
use crate::sexp::Ingot;

/// Structured progress entry for PROGRESS.md
pub struct ProgressEntry<'a> {
    pub ingot: &'a Ingot,
    pub heat: u8,
    pub files_changed: Vec<String>,
    pub learnings: Option<String>,
}

/// Append a structured progress entry to PROGRESS.md
pub fn append_entry(entry: &ProgressEntry) -> Result<(), std::io::Error> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)?;

    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M");
    writeln!(f)?;
    writeln!(
        f,
        "## {ts} [{id}] gr:{grade} skill:{skill}",
        id = entry.ingot.id,
        grade = entry.ingot.grade,
        skill = entry.ingot.skill,
    )?;
    writeln!(f, "- {}", entry.ingot.work)?;
    writeln!(f, "- heats: {}", entry.heat)?;

    if !entry.files_changed.is_empty() {
        writeln!(f, "- files: {}", entry.files_changed.join(", "))?;
    }

    if let Some(ref learnings) = entry.learnings {
        writeln!(f, "- learned: {learnings}")?;
    }

    Ok(())
}

/// Get list of files changed since last commit
pub fn files_changed_since_last_commit() -> Vec<String> {
    std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .map(|l| l.to_string())
                        .collect(),
                )
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Initialize the codebase patterns section in PROGRESS.md (Ralph-inspired)
pub fn init_patterns_section() -> Result<(), std::io::Error> {
    let ledger_path = std::path::Path::new(LEDGER);
    if ledger_path.exists() {
        let content = std::fs::read_to_string(ledger_path)?;
        if content.contains("## Codebase Patterns") {
            return Ok(());
        }
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)?;

    writeln!(f, "\n## Codebase Patterns")?;
    writeln!(f, "_Populated during forging. Helps future ingots understand the codebase._\n")?;

    Ok(())
}

/// Append a pattern observation to the patterns section
pub fn append_pattern(pattern: &str) -> Result<(), std::io::Error> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LEDGER)?;

    writeln!(f, "- {pattern}")?;
    Ok(())
}
