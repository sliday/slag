use crate::config::{BLUEPRINT, MAX_ITERATE, ORE_FILE};
use crate::error::SlagError;
use crate::flux;
use crate::smith::{self, Smith};
use crate::tui;

pub async fn direct_proof(smith: &dyn Smith, commission: &str) -> Result<Option<String>, SlagError> {
    let raw = smith.invoke(&format!(
        "Choose the work scope for a fresh, empty project.\n\
         COMMISSION:\n{commission}\n\n\
         Use direct=true only for a local utility with at most four product files, \
         a deterministic test command, and no external service, UI, deployment, \
         or architectural research. Otherwise use direct=false.\n\
         Return only JSON: {{\"direct\":true,\"proof\":\"test command\"}} \
         or {{\"direct\":false}}. Do not use tools. \
         The proof must test the requested behavior. Do not invent acceptance rules."
    )).await?;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
        return Ok(None);
    };
    let proof = value["proof"].as_str().unwrap_or_default().trim();
    if value["direct"] == true && !proof.is_empty() && proof != "true" {
        Ok(Some(proof.to_string()))
    } else {
        Ok(None)
    }
}

/// Phase 1: Analyze the commission (PRD.md) and produce a BLUEPRINT.md
pub async fn run(smith: &dyn Smith) -> Result<(), SlagError> {
    run_with_guidance(smith, None).await
}

/// Survey again, told what the last blueprint failed to cover. `guidance`
/// is a warden's gap, fed back verbatim: it is the one thing this pass has
/// to fix, and paraphrasing it loses the specificity that makes it useful.
pub async fn run_with_guidance(
    smith: &dyn Smith,
    guidance: Option<&str>,
) -> Result<(), SlagError> {
    tui::header("SURVEYOR · deep analysis");

    let ore = std::fs::read_to_string(ORE_FILE)
        .map_err(|_| SlagError::NoOre)?;

    let mut prompt = flux::surveyor_prompt(&ore);
    if let Some(gap) = guidance {
        prompt.push_str(&format!(
            "\n\nA reviewer rejected the previous blueprint for this reason. \
             The new one must cover it:\n{gap}"
        ));
    }
    log_to_file("SURVEY_PROMPT", &prompt);

    let spinner = tui::spinner("surveying...");
    // A smith that reached for a tool ends on a finish summary, so this can
    // come back as "Completed survey and created the blueprint" where the
    // blueprint was expected. Ask once more, plainly, before failing: the
    // guard below stops that sentence reaching disk, but on a cold project
    // there is no blueprint to fall back on and the run dies instead.
    let raw = crate::smith::invoke_document(
        smith,
        &prompt,
        flux::DOCUMENT_ONLY_REMINDER,
        flux::looks_like_a_document,
    )
    .await
    .map_err(|e| {
        spinner.finish_and_clear();
        SlagError::SurveyFailed(e)
    })?;
    spinner.finish_and_clear();

    log_to_file("SURVEY_RAW", &raw);

    // Self-iterate if questions detected
    let raw = smith::self_iterate(smith, raw, MAX_ITERATE).await?;

    // Refuse to write a summary over the blueprint. A smith that explored
    // with tools finishes on a sentence about its work, and writing that
    // here replaces the document every later phase reads. Keeping a thin
    // blueprint beats replacing it with one line about a blueprint.
    if !flux::looks_like_a_document(&raw) {
        if std::path::Path::new(BLUEPRINT).exists() {
            tui::status_line("!", tui::WARM, "surveyor returned a summary; keeping the existing blueprint");
            return Ok(());
        }
        return Err(SlagError::SurveyFailed(
            "the surveyor returned a summary, not a blueprint".to_string(),
        ));
    }
    std::fs::write(BLUEPRINT, &raw)?;
    tui::status_line("█", tui::PURE, &format!("Blueprint: {BLUEPRINT}"));

    // Show preview
    if !tui::is_quiet() {
        println!();
        let lines: Vec<&str> = raw.lines().collect();
        for line in lines.iter().take(20) {
            println!("  {}{line}{}", super::fg(tui::COLD), super::reset());
        }
        if lines.len() > 20 {
            println!(
                "\n  {}... +{} lines{}",
                super::fg(tui::COLD),
                lines.len() - 20,
                super::reset()
            );
        }
    }

    Ok(())
}

fn log_to_file(label: &str, content: &str) {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = format!("{}/{ts}_{label}.log", crate::config::LOG_DIR);
    let _ = std::fs::write(&path, content);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_scope_requires_a_test_command_and_explicit_routing() {
        for (reply, expected) in [
            (r#"{"direct":true,"proof":"python3 -m unittest"}"#, Some("python3 -m unittest")),
            (r#"{"direct":false}"#, None),
            (r#"{"direct":true,"proof":"true"}"#, None),
            (r#"{"direct":true,"proof":""}"#, None),
            ("A small project.", None),
        ] {
            let smith = crate::smith::mock::MockSmith::fixed(reply);
            assert_eq!(direct_proof(&smith, "local utility with tests").await.unwrap().as_deref(), expected);
            assert_eq!(smith.call_count(), 1);
        }
    }
}
