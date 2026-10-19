use crate::config::{BLUEPRINT, MAX_ITERATE, ORE_FILE};
use crate::error::SlagError;
use crate::flux;
use crate::smith::{self, Smith};
use crate::tui;

pub async fn direct_proof(smith: &dyn Smith, commission: &str) -> Result<Option<super::direct::DirectPlan>, SlagError> {
    let raw = smith.invoke(&super::direct::scope_prompt(commission)).await?;
    if let Some(plan) = super::direct::parse_scope(&raw, commission) {
        return Ok(Some(plan));
    }
    if let Some(prompt) = super::direct::reference_mapping_prompt(commission, &raw) {
        let reference = smith.invoke(&prompt).await?;
        return super::direct::complete_reference(commission, &raw, &reference).map(Some)
            .ok_or_else(|| SlagError::SurveyFailed("the named reference has no valid comparison contract".into()));
    }
    let Some(prompt) = super::direct::scope_repair_prompt(commission, &raw) else { return Ok(None) };
    let repaired = smith.invoke(&prompt).await?;
    Ok(super::direct::parse_scope(&repaired, commission))
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
        for (reply, expected, calls) in [
            (r#"{"direct":true,"proof":"python3 -m unittest","criteria":["The utility passes its requested unittest."]}"#, Some("python3 -m unittest"), 1),
            (r#"{"direct":false}"#, None, 1),
            (r#"{"direct":true,"proof":"true"}"#, None, 2),
            (r#"{"direct":true,"proof":""}"#, None, 2),
            ("A small project.", None, 1),
        ] {
            let smith = crate::smith::mock::MockSmith::fixed(reply);
            let plan = direct_proof(&smith, "local utility with tests").await.unwrap();
            assert_eq!(plan.as_ref().map(|plan| plan.proof.as_str()), expected);
            assert_eq!(smith.call_count(), calls);
        }
    }

    #[tokio::test]
    async fn an_invalid_direct_contract_gets_one_repair_attempt() {
        let invalid = r#"{"direct":true,"proof":"python3 -m unittest","criteria":["The CLI counts words."],"commands":["python3 wc.py fixture.txt > output.json"]}"#;
        let valid = r#"{"direct":true,"proof":"python3 -m unittest","criteria":["The CLI counts words."],"commands":["python3 wc.py fixture.txt"]}"#;
        let smith = crate::smith::mock::MockSmith::new(vec![invalid.into(), valid.into()]);
        let plan = direct_proof(&smith, "Create a word counting CLI.").await.unwrap().unwrap();
        assert_eq!(plan.commands, ["python3 wc.py fixture.txt"]);
        assert_eq!(smith.call_count(), 2);

        let smith = crate::smith::mock::MockSmith::fixed(invalid);
        assert!(direct_proof(&smith, "Create a word counting CLI.").await.unwrap().is_none());
        assert_eq!(smith.call_count(), 2);
    }

    #[tokio::test]
    async fn an_omitted_reference_gets_a_bounded_mapping_request() {
        let scope = serde_json::json!({
            "direct": true, "proof": "python3 -m unittest", "criteria": ["Copy file contents like cat."],
            "commands": ["python3 copy.py fixtures/sample.txt"]
        }).to_string();
        let reference = serde_json::json!({"actual": ["python3", "copy.py", "{input}"], "command": ["cat", "{input}"], "comparison": "bytes"}).to_string();
        let mut wrapped: serde_json::Value = serde_json::from_str(&scope).unwrap();
        wrapped["reference"] = serde_json::from_str(&reference).unwrap();
        for reply in [reference, wrapped.to_string()] {
            let smith = crate::smith::mock::MockSmith::new(vec![scope.clone(), reply]);
            let plan = direct_proof(&smith, "A file CLI like cat with tests.").await.unwrap().unwrap();
            assert_eq!(plan.fixtures.len(), 2);
            assert_eq!(plan.commands.len(), 3);
            assert_eq!(smith.call_count(), 2);
        }
        let smith = crate::smith::mock::MockSmith::fixed(&scope);
        assert!(direct_proof(&smith, "A file CLI like cat with tests.").await.is_err());
        assert_eq!(smith.call_count(), 2);
    }

    #[tokio::test]
    async fn scope_provider_errors_do_not_trigger_repair() {
        let smith = crate::smith::mock::MockSmith::failing();
        assert!(direct_proof(&smith, "Create a local utility.").await.is_err());
        assert_eq!(smith.call_count(), 1);
    }
}
