use crate::config::{BLUEPRINT, CRUCIBLE, MAX_ITERATE, ORE_FILE};
use crate::crucible::{self, Crucible};
use crate::error::SlagError;
use crate::flux;
use crate::smith::{self, Smith};
use crate::tui;

/// Phase 2: Read blueprint and produce S-expression ingots in PLAN.md
pub async fn run(smith: &dyn Smith) -> Result<(), SlagError> {
    tui::header("FOUNDER · casting mold");

    let ore = std::fs::read_to_string(ORE_FILE)
        .map_err(|_| SlagError::NoOre)?;
    let blueprint = std::fs::read_to_string(BLUEPRINT)
        .unwrap_or_else(|_| "No blueprint".into());

    let prompt = with_briefing_rules(&flux::founder_prompt(&ore, &blueprint));
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
    if !tui::is_quiet() {
        println!();
        println!(
            "  {}{:<5} {:<3} {:<4} {:<7} {}{}",
            super::fg(tui::COLD),
            "ID",
            "GR",
            "SOLO",
            "SKILL",
            "WORK",
            super::reset()
        );
        for (i, ingot) in ingots.iter().enumerate() {
            if i >= 10 {
                break;
            }
            let solo_sym = if ingot.solo { "∥" } else { "→" };
            println!(
                "  {}{:<5}{} {:<3} {:<4} {:<7} {}",
                super::fg(tui::HOT),
                ingot.id,
                super::reset(),
                ingot.grade,
                solo_sym,
                ingot.skill,
                tui::truncate(&ingot.work, 38),
            );
        }
        if count > 10 {
            println!("  {}+{} more{}", super::fg(tui::COLD), count - 10, super::reset());
        }
    }

    Ok(())
}

/// Ingots ARE zero-context subagent prompts: the smith forging one sees
/// nothing but the ingot's :work text. These rules ride the founder prompt
/// so every :work briefs like a colleague joining with zero context —
/// which is what raises the first-heat pass rate.
const BRIEFING_RULES: &str = "\
BRIEFING RULES (each :work is read by a smith with ZERO context — \
it sees only the ingot, never this commission or blueprint):\n\
- State the GOAL and WHY it matters, not just the mechanical step.\n\
- Name what is RULED OUT: approaches to avoid, files not to touch.\n\
- Use CONCRETE file paths (src/api/routes.js), never 'the config' or 'that module'.\n\
- Lookups get the EXACT command to run; investigations get the QUESTION to answer.\n\
- Never delegate understanding: the ingot carries every fact needed to start cold.";

/// Append the zero-context briefing rules to the founder prompt.
fn with_briefing_rules(base: &str) -> String {
    format!("{base}\n\n{BRIEFING_RULES}")
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn briefing_rules_cover_the_zero_context_contract() {
        for required in [
            "ZERO context",
            "GOAL",
            "WHY",
            "RULED OUT",
            "file paths",
            "EXACT command",
            "QUESTION",
            "Never delegate understanding",
        ] {
            assert!(
                BRIEFING_RULES.contains(required),
                "briefing rules must mention {required:?}"
            );
        }
    }

    #[test]
    fn with_briefing_rules_appends_after_the_base_prompt() {
        let prompt = with_briefing_rules("BASE PROMPT");
        assert!(prompt.starts_with("BASE PROMPT"), "{prompt}");
        assert!(prompt.ends_with(BRIEFING_RULES), "{prompt}");
        assert_eq!(
            prompt.matches("BRIEFING RULES").count(),
            1,
            "rules must appear exactly once"
        );
    }

    #[test]
    fn founder_prompt_carries_briefing_rules() {
        let prompt = with_briefing_rules(&flux::founder_prompt("ore", "blueprint"));
        assert!(prompt.contains("OUTPUT: S-expressions only"), "{prompt}");
        assert!(prompt.contains("BRIEFING RULES"), "{prompt}");
    }
}
