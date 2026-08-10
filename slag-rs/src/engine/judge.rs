//! judge — the assayer for twin-cast duels.
//!
//! Proofs gate, the judge ranks: both casts already passed `:proof`, so the
//! judge scores quality only — correctness risks beyond the proof, clarity,
//! minimal diff, design fit. Position swap (hermes LLM-judge bias
//! mitigation): the model rules twice, A-then-B and B-then-A. Agreement
//! averages the scores; disagreement collapses to a margin-0 tie so the
//! caller never early-stops on a biased verdict. The judge holds no tools
//! and never writes code.

use serde::Deserialize;

// `crate::engine`, not `super`: this file is mounted via a #[path] shim in
// tools.rs while engine/mod.rs is frozen. Revert to `super` when it moves.
use crate::engine::{ChatMessage, ChatRequest, Effort, Provider, Verdict};
use crate::error::SlagError;

const MAX_DIFF_CHARS: usize = 30_000;
const TRUNCATION_MARKER: &str = "\n… [diff truncated: tail cut at 30000 chars]";
const RETRY_NUDGE: &str = "reply with ONLY the JSON object";

/// One proof-passing cast presented to the judge.
pub struct CastResult {
    pub diff: String,
    pub proof_output: String,
}

/// Compare two working casts; return the assayer's verdict.
///
/// `images` = (screenshot_a, screenshot_b) data URLs for visual assay;
/// attached to the first call's user message only.
pub async fn assay(
    provider: &dyn Provider,
    model: &str,
    work: &str,
    a: &CastResult,
    b: &CastResult,
    prior_critique: Option<&str>,
    images: Option<(String, String)>,
) -> Result<Verdict, SlagError> {
    let first_images = images.map(|(img_a, img_b)| vec![img_a, img_b]);
    let first = judge_once(provider, model, work, a, b, prior_critique, first_images).await?;
    // Swapped pass: cast B presented under label A and vice versa.
    let swapped = judge_once(provider, model, work, b, a, prior_critique, None).await?;
    let second = unswap(swapped);
    // The swapped pass wrote its critique with A/B seats reversed; flag it
    // so downstream readers (next-round smiths, the judge's prior_critique)
    // do not attribute observations to the wrong cast.
    let critique = format!("{}\n{SWAP_NOTE}\n{}", first.critique, second.critique);

    if first.winner == second.winner {
        return Ok(Verdict {
            winner: first.winner,
            score_a: midpoint(first.score_a, second.score_a),
            score_b: midpoint(first.score_b, second.score_b),
            critique,
        });
    }

    // Positional disagreement: the ruling tracked seat order, not quality.
    // Equal scores make margin() zero — the caller treats it as a tie.
    let level = midpoint(
        midpoint(first.score_a, second.score_a),
        midpoint(first.score_b, second.score_b),
    );
    Ok(Verdict {
        winner: 'a',
        score_a: level,
        score_b: level,
        critique,
    })
}

/// Marker inserted before the swapped pass's critique text.
const SWAP_NOTE: &str =
    "[note: the following critique was written with seats swapped — its \"cast A\" is the real \
cast B and vice versa]";

/// One judging pass: `first` sits in seat A, `second` in seat B.
/// Malformed JSON gets one retry nudge, then a provider error.
async fn judge_once(
    provider: &dyn Provider,
    model: &str,
    work: &str,
    first: &CastResult,
    second: &CastResult,
    prior_critique: Option<&str>,
    images: Option<Vec<String>>,
) -> Result<Verdict, SlagError> {
    let mut user = ChatMessage::user(rubric_prompt(work, first, second, prior_critique));
    user.images = images;
    let mut messages = vec![ChatMessage::system(SYSTEM_PROMPT), user];

    for attempt in 0..2 {
        let resp = provider
            .chat(ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: vec![],
                effort: Some(Effort::High),
                max_tokens: None,
            })
            .await?;

        if let Some(verdict) = parse_verdict(&resp.content) {
            return Ok(verdict);
        }
        if attempt == 0 {
            messages.push(ChatMessage::assistant(resp.content, None));
            messages.push(ChatMessage::user(RETRY_NUDGE));
        }
    }

    Err(SlagError::Provider(
        "judge returned malformed verdict JSON after retry".into(),
    ))
}

const SYSTEM_PROMPT: &str = "You are the assayer for slag's forge. Two casts solved the same task \
and both already passed the acceptance proof — correctness at the proof level is settled. Judge \
them ONLY on quality: correctness risks beyond the proof, clarity, minimal diff, design fit. \
You never write code. Your entire reply must be a single JSON object.";

fn rubric_prompt(
    work: &str,
    first: &CastResult,
    second: &CastResult,
    prior_critique: Option<&str>,
) -> String {
    let mut prompt = format!("## Task\n{work}\n");
    if let Some(critique) = prior_critique {
        prompt.push_str(&format!(
            "\n## Prior round critique\nBoth casts received this critique last round; weigh how \
well each addressed it:\n{critique}\n"
        ));
    }
    prompt.push_str(&format!(
        "\n## Cast A\n### Diff\n{}\n### Proof output\n{}\n\
\n## Cast B\n### Diff\n{}\n### Proof output\n{}\n\
\n## Ruling\n\
Score each cast 0-100 on quality: correctness risks beyond the proof, clarity, minimal diff, \
design fit. The critique must state what the LOSER did better than the winner — it seeds the \
next round.\n\
Reply with ONLY this JSON object as your entire reply:\n\
{{\"winner\":\"a\"|\"b\",\"score_a\":<0-100>,\"score_b\":<0-100>,\"critique\":\"...\"}}",
        truncate_diff(&first.diff),
        first.proof_output,
        truncate_diff(&second.diff),
        second.proof_output,
    ));
    prompt
}

/// Cap a diff at `MAX_DIFF_CHARS`, marking the cut tail.
fn truncate_diff(diff: &str) -> String {
    if diff.len() <= MAX_DIFF_CHARS {
        return diff.to_string();
    }
    let mut end = MAX_DIFF_CHARS;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &diff[..end], TRUNCATION_MARKER)
}

#[derive(Deserialize)]
struct RawVerdict {
    winner: String,
    score_a: u16,
    score_b: u16,
    critique: String,
}

/// Lenient JSON extraction: take the outermost brace span, parse strictly.
fn parse_verdict(content: &str) -> Option<Verdict> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end < start {
        return None;
    }
    let raw: RawVerdict = serde_json::from_str(&content[start..=end]).ok()?;
    let winner = match raw.winner.trim().to_lowercase().as_str() {
        "a" | "cast a" => 'a',
        "b" | "cast b" => 'b',
        _ => return None,
    };
    Some(Verdict {
        winner,
        score_a: raw.score_a.min(100) as u8,
        score_b: raw.score_b.min(100) as u8,
        critique: raw.critique,
    })
}

/// Undo the seat swap of the second pass: its "a" is the real cast B.
fn unswap(v: Verdict) -> Verdict {
    Verdict {
        winner: if v.winner == 'a' { 'b' } else { 'a' },
        score_a: v.score_b,
        score_b: v.score_a,
        critique: v.critique,
    }
}

fn midpoint(x: u8, y: u8) -> u8 {
    ((x as u16 + y as u16) / 2) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{FinishReason, NormalizedResponse, Usage};
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Scripted judge: pops canned replies, records every request.
    struct MockJudge {
        replies: Mutex<VecDeque<String>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl MockJudge {
        fn new(replies: &[&str]) -> Self {
            Self {
                replies: Mutex::new(replies.iter().map(|s| s.to_string()).collect()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Provider for MockJudge {
        fn chat(
            &self,
            req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            self.requests.lock().unwrap().push(req);
            let content = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("mock judge ran out of replies");
            Box::pin(async move {
                Ok(NormalizedResponse {
                    content,
                    tool_calls: vec![],
                    finish_reason: FinishReason::Stop,
                    reasoning: None,
                    reasoning_details: None,
                    usage: Usage::default(),
                })
            })
        }
    }

    fn cast(diff: &str, proof: &str) -> CastResult {
        CastResult {
            diff: diff.into(),
            proof_output: proof.into(),
        }
    }

    #[tokio::test]
    async fn agreement_averages_scores_and_concats_critiques() {
        // Second call sees swapped seats, so wire winner "b" = real cast A.
        let judge = MockJudge::new(&[
            r#"{"winner":"a","score_a":90,"score_b":70,"critique":"B had tighter naming"}"#,
            r#"{"winner":"b","score_a":72,"score_b":88,"critique":"loser was more defensive"}"#,
        ]);

        let verdict = assay(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("diff-a", "ok"),
            &cast("diff-b", "ok"),
            None,
            None,
        )
        .await
        .expect("verdict");

        assert_eq!(verdict.winner, 'a');
        assert_eq!(verdict.score_a, 89); // (90 + 88) / 2
        assert_eq!(verdict.score_b, 71); // (70 + 72) / 2
        assert!(verdict.critique.contains("B had tighter naming"));
        assert!(verdict.critique.contains("loser was more defensive"));
        // The swapped pass's critique used reversed A/B labels; the note
        // must sit between the two so readers re-map them.
        let note_at = verdict.critique.find(SWAP_NOTE).expect("swap note present");
        assert!(note_at > verdict.critique.find("tighter naming").unwrap());
        assert!(note_at < verdict.critique.find("more defensive").unwrap());

        // Judge calls carry no tools and demand high effort.
        for req in judge.requests() {
            assert!(req.tools.is_empty());
            assert_eq!(req.effort, Some(Effort::High));
        }
    }

    #[tokio::test]
    async fn disagreement_yields_margin_zero() {
        // Both passes crown seat A — a positional ruling, not a quality one.
        // Unswapped, the second pass backs real cast B: disagreement.
        let judge = MockJudge::new(&[
            r#"{"winner":"a","score_a":80,"score_b":60,"critique":"c1"}"#,
            r#"{"winner":"a","score_a":80,"score_b":60,"critique":"c2"}"#,
        ]);

        let verdict = assay(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("diff-a", "ok"),
            &cast("diff-b", "ok"),
            None,
            None,
        )
        .await
        .expect("verdict");

        assert_eq!(verdict.winner, 'a');
        assert_eq!(verdict.margin(), 0);
        assert_eq!(verdict.score_a, verdict.score_b);
        assert!(verdict.critique.contains("c1"));
        assert!(verdict.critique.contains("c2"));
    }

    #[tokio::test]
    async fn malformed_json_retries_once_with_nudge() {
        let judge = MockJudge::new(&[
            "I think cast A wins because it is cleaner.",
            r#"{"winner":"a","score_a":85,"score_b":75,"critique":"c1"}"#,
            r#"{"winner":"b","score_a":75,"score_b":85,"critique":"c2"}"#,
        ]);

        let verdict = assay(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("diff-a", "ok"),
            &cast("diff-b", "ok"),
            None,
            None,
        )
        .await
        .expect("verdict after retry");

        assert_eq!(verdict.winner, 'a');
        let requests = judge.requests();
        assert_eq!(requests.len(), 3);
        // The retry replays the bad reply and appends the nudge.
        let retry = &requests[1].messages;
        assert_eq!(retry.last().unwrap().content, RETRY_NUDGE);
        assert_eq!(retry[retry.len() - 2].role, "assistant");
    }

    #[tokio::test]
    async fn malformed_json_twice_is_provider_error() {
        let judge = MockJudge::new(&["not json", "still not json"]);

        let err = assay(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("diff-a", "ok"),
            &cast("diff-b", "ok"),
            None,
            None,
        )
        .await
        .expect_err("must fail");

        assert!(matches!(err, SlagError::Provider(_)), "got: {err}");
        assert_eq!(judge.requests().len(), 2);
    }

    #[tokio::test]
    async fn long_diffs_are_truncated_with_tail_marker() {
        let long_diff = "x".repeat(MAX_DIFF_CHARS + 5_000);
        let judge = MockJudge::new(&[
            r#"{"winner":"a","score_a":80,"score_b":70,"critique":"c1"}"#,
            r#"{"winner":"b","score_a":70,"score_b":80,"critique":"c2"}"#,
        ]);

        assay(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast(&long_diff, "ok"),
            &cast("diff-b", "ok"),
            None,
            None,
        )
        .await
        .expect("verdict");

        let prompt = judge.requests()[0].messages[1].content.clone();
        assert!(prompt.contains(TRUNCATION_MARKER));
        assert!(!prompt.contains(&long_diff), "full diff must not survive");
    }

    #[tokio::test]
    async fn position_swap_puts_b_first_in_second_call() {
        let judge = MockJudge::new(&[
            r#"{"winner":"a","score_a":80,"score_b":70,"critique":"c1"}"#,
            r#"{"winner":"b","score_a":70,"score_b":80,"critique":"c2"}"#,
        ]);

        assay(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("DIFF-ALPHA", "ok"),
            &cast("DIFF-BETA", "ok"),
            None,
            Some(("data:image/png;base64,AA".into(), "data:image/png;base64,BB".into())),
        )
        .await
        .expect("verdict");

        let requests = judge.requests();
        let first_prompt = &requests[0].messages[1].content;
        let second_prompt = &requests[1].messages[1].content;

        // First call: A then B. Second call: seats swapped, B first.
        assert!(first_prompt.find("DIFF-ALPHA").unwrap() < first_prompt.find("DIFF-BETA").unwrap());
        assert!(
            second_prompt.find("DIFF-BETA").unwrap() < second_prompt.find("DIFF-ALPHA").unwrap()
        );

        // Screenshots ride the first call's user message only.
        assert_eq!(
            requests[0].messages[1].images.as_deref(),
            Some(&["data:image/png;base64,AA".to_string(), "data:image/png;base64,BB".to_string()][..])
        );
        assert!(requests[1].messages[1].images.is_none());
    }

    #[test]
    fn parse_verdict_extracts_json_from_noise() {
        let verdict = parse_verdict(
            "```json\n{\"winner\":\"b\",\"score_a\":40,\"score_b\":200,\"critique\":\"c\"}\n```",
        )
        .expect("parsed");
        assert_eq!(verdict.winner, 'b');
        assert_eq!(verdict.score_a, 40);
        assert_eq!(verdict.score_b, 100); // clamped
        assert!(parse_verdict("no braces at all").is_none());
        assert!(parse_verdict(r#"{"winner":"c","score_a":1,"score_b":2,"critique":""}"#).is_none());
    }
}
