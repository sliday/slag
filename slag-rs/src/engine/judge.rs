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
use crate::engine::compact::{NO_TOOLS_PREAMBLE, NO_TOOLS_TRAILER};
use crate::engine::{ChatMessage, ChatRequest, Effort, Provider, RetryPolicy, Role, Verdict};
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

/// Three-way verdict for a triple-cast round: pairwise round-robin
/// (AB, AC, BC), each pair judged by `assay` with its position swap.
#[derive(Debug)]
pub struct TriVerdict {
    /// 'a' | 'b' | 'c'; on a tie, 'a' with `tie` set.
    pub winner: char,
    /// Min winning margin across the winner's pairs; 0 on a tie.
    pub margin: u8,
    /// Winner's average score across its two pairs (plateau tracking).
    pub winner_score: u8,
    /// Rock-paper-scissors: every cast took exactly one pair. The caller
    /// re-casts (or crowns 'a' on the final round) instead of trusting
    /// a cyclic ranking.
    pub tie: bool,
    /// All three pair critiques, labelled, for the next round's casts.
    pub critique: String,
}

/// Compare three working casts pairwise; the cast with the most pairwise
/// wins takes the round. Margin is the winner's weakest pairwise margin —
/// a cast that barely edged one rival has not converged. Cyclic results
/// (1 win each) surface as a tie with margin 0.
pub async fn assay3(
    provider: &dyn Provider,
    model: &str,
    work: &str,
    a: &CastResult,
    b: &CastResult,
    c: &CastResult,
    prior_critique: Option<&str>,
) -> Result<TriVerdict, SlagError> {
    // Seat labels inside each pair are always a/b; map back per pair.
    let pairs: [(char, &CastResult, char, &CastResult); 3] =
        [('a', a, 'b', b), ('a', a, 'c', c), ('b', b, 'c', c)];

    let mut wins = [('a', 0u8), ('b', 0u8), ('c', 0u8)];
    let mut margins: Vec<(char, u8, u8)> = Vec::with_capacity(3); // (winner, margin, score)
    let mut critique = String::new();

    for (first_label, first, second_label, second) in pairs {
        let v = assay(provider, model, work, first, second, prior_critique, None).await?;
        // A margin-0 verdict is `assay`'s positional-disagreement collapse
        // (or a dead-even ruling): it names seat A only as a placeholder,
        // not as a quality winner. Crediting it a pairwise win would bias
        // the round-robin toward earlier labels, so a levelled pair
        // credits no one — only decisive pairs rank.
        if v.margin() > 0 {
            let (winner, winner_score) = if v.winner == 'a' {
                (first_label, v.score_a)
            } else {
                (second_label, v.score_b)
            };
            if let Some(entry) = wins.iter_mut().find(|(l, _)| *l == winner) {
                entry.1 += 1;
            }
            margins.push((winner, v.margin(), winner_score));
        }
        critique.push_str(&format!(
            "\n[pair {} vs {} — this pair's \"cast A\" is cast {} and \"cast B\" is cast {}]\n{}\n",
            first_label.to_uppercase(),
            second_label.to_uppercase(),
            first_label.to_uppercase(),
            second_label.to_uppercase(),
            v.critique,
        ));
    }

    let best = wins.iter().map(|(_, w)| *w).max().unwrap_or(0);
    if best < 2 {
        // Cycle (a beat b, b beat c, c beat a) — or too many levelled
        // pairs for any cast to prove itself across two rivals.
        return Ok(TriVerdict { winner: 'a', margin: 0, winner_score: 0, tie: true, critique });
    }
    let winner = wins.iter().find(|(_, w)| *w == best).map(|(l, _)| *l).unwrap_or('a');
    let won: Vec<&(char, u8, u8)> = margins.iter().filter(|(w, _, _)| *w == winner).collect();
    let margin = won.iter().map(|(_, m, _)| *m).min().unwrap_or(0);
    let winner_score = {
        let sum: u16 = won.iter().map(|(_, _, s)| *s as u16).sum();
        (sum / won.len().max(1) as u16) as u8
    };
    Ok(TriVerdict { winner, margin, winner_score, tie: false, critique })
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
    // Consequence-first no-tools framing (item 43): the preamble leads the
    // system prompt; rubric_prompt appends the matching trailer.
    let mut messages = vec![
        ChatMessage::system(format!("{NO_TOOLS_PREAMBLE} {SYSTEM_PROMPT}")),
        user,
    ];

    for attempt in 0..2 {
        let resp = provider
            .chat(ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: vec![],
                effort: Some(Effort::High),
                max_tokens: None,
                role: Role::Judge,
                retry: RetryPolicy::side(),
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
    prompt.push_str(&format!("\n{NO_TOOLS_TRAILER}"));
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

/// One gate's answer for a prompt hook (item 74).
#[derive(Debug)]
pub struct Ruling {
    pub block: bool,
    pub reason: String,
}

const RULE_PROMPT: &str = "You are a gate in slag's forge. You are shown one lifecycle event and \
one instruction describing what to refuse. Decide whether to allow the event or block it. Blocking \
stops real work, so block only when the instruction clearly calls for it; when in doubt, allow and \
say why in the reason. You never write code and you never call tools. Your entire reply must be a \
single JSON object.";

/// Rule on one lifecycle event: allow it, or block it with a reason.
///
/// The cheap sibling of `assay` — `Effort::Low`, because this fires on every
/// matching event and a gate costing more than the work it guards does not
/// get used. Malformed JSON gets one retry nudge, then an error. The caller
/// (`hooks::run_prompt`) maps that error to `CODE_FAILED`, never to a block:
/// a broken gate must not wedge a forge.
pub async fn rule(
    provider: &dyn Provider,
    model: &str,
    instruction: &str,
    payload: &str,
) -> Result<Ruling, SlagError> {
    let mut messages = vec![
        ChatMessage::system(format!("{NO_TOOLS_PREAMBLE} {RULE_PROMPT}")),
        ChatMessage::user(rule_prompt(instruction, payload)),
    ];

    for attempt in 0..2 {
        let resp = provider
            .chat(ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: vec![],
                effort: Some(Effort::Low),
                max_tokens: None,
                role: Role::Judge,
                retry: RetryPolicy::side(),
            })
            .await?;

        if let Some(ruling) = parse_ruling(&resp.content) {
            return Ok(ruling);
        }
        if attempt == 0 {
            messages.push(ChatMessage::assistant(resp.content, None));
            messages.push(ChatMessage::user(RETRY_NUDGE));
        }
    }

    Err(SlagError::Provider(
        "hook gate returned malformed ruling JSON after retry".into(),
    ))
}

fn rule_prompt(instruction: &str, payload: &str) -> String {
    format!(
        "## Instruction\n{instruction}\n\
\n## Event payload (JSON)\n{payload}\n\
\n## Ruling\n\
Reply with ONLY this JSON object as your entire reply:\n\
{{\"decision\":\"allow\"|\"block\",\"reason\":\"...\"}}\n\
{NO_TOOLS_TRAILER}"
    )
}

#[derive(Deserialize)]
struct RawRuling {
    decision: String,
    #[serde(default)]
    reason: String,
}

/// Lenient JSON extraction, same discipline as `parse_verdict`: outermost
/// brace span, then a strict parse. An unrecognized decision is malformed,
/// never a silent allow.
fn parse_ruling(content: &str) -> Option<Ruling> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end < start {
        return None;
    }
    let raw: RawRuling = serde_json::from_str(&content[start..=end]).ok()?;
    let block = match raw.decision.trim().to_lowercase().as_str() {
        "allow" => false,
        "block" => true,
        _ => return None,
    };
    Some(Ruling {
        block,
        reason: raw.reason,
    })
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
                    model: None,
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

    #[tokio::test]
    async fn three_way_round_robin_ranks_by_pairwise_wins() {
        // Pairs run AB, AC, BC; each pair judges twice (position swap).
        // A beats B (margin 20) and C (margin 10); B beats C.
        let judge = MockJudge::new(&[
            r#"{"winner":"a","score_a":90,"score_b":70,"critique":"ab"}"#,
            r#"{"winner":"b","score_a":70,"score_b":90,"critique":"ab-swap"}"#,
            r#"{"winner":"a","score_a":85,"score_b":75,"critique":"ac"}"#,
            r#"{"winner":"b","score_a":75,"score_b":85,"critique":"ac-swap"}"#,
            r#"{"winner":"a","score_a":80,"score_b":60,"critique":"bc"}"#,
            r#"{"winner":"b","score_a":60,"score_b":80,"critique":"bc-swap"}"#,
        ]);

        let verdict = assay3(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("DIFF-A", "ok"),
            &cast("DIFF-B", "ok"),
            &cast("DIFF-C", "ok"),
            None,
        )
        .await
        .expect("verdict");

        assert_eq!(verdict.winner, 'a');
        assert!(!verdict.tie);
        // Weakest pairwise margin wins: min(20 vs B, 10 vs C).
        assert_eq!(verdict.margin, 10);
        // Winner's average score across its pairs: (90 + 85) / 2.
        assert_eq!(verdict.winner_score, 87);
        // All three pair critiques ride along, labelled with seat maps.
        for label in ["[pair A vs B", "[pair A vs C", "[pair B vs C"] {
            assert!(verdict.critique.contains(label), "missing {label}");
        }

        // Round-robin + rotation: 6 calls, each pair seated both ways.
        let requests = judge.requests();
        assert_eq!(requests.len(), 6);
        let prompt = |i: usize| requests[i].messages[1].content.clone();
        let order = |p: &str, x: &str, y: &str| p.find(x).unwrap() < p.find(y).unwrap();
        assert!(order(&prompt(0), "DIFF-A", "DIFF-B") && !prompt(0).contains("DIFF-C"));
        assert!(order(&prompt(1), "DIFF-B", "DIFF-A"));
        assert!(order(&prompt(2), "DIFF-A", "DIFF-C") && !prompt(2).contains("DIFF-B"));
        assert!(order(&prompt(3), "DIFF-C", "DIFF-A"));
        assert!(order(&prompt(4), "DIFF-B", "DIFF-C") && !prompt(4).contains("DIFF-A"));
        assert!(order(&prompt(5), "DIFF-C", "DIFF-B"));
    }

    #[tokio::test]
    async fn positional_disagreement_pairs_credit_no_pairwise_win() {
        // Pairs AB and AC both collapse to positional disagreements (each
        // judge pass crowned its seat A — unswapped, that is a split
        // verdict, margin 0). Only BC is a genuine ruling: B wins big.
        // Before the fix, the two collapsed pairs each credited cast 'a'
        // a "win" (wins a:2, b:1) and 'a' took the round on pure seat
        // order. Levelled pairs must credit no one → nobody reaches two
        // wins → the round is a tie, never an 'a' coronation.
        let judge = MockJudge::new(&[
            // AB: both passes say seat A → disagreement after unswap.
            r#"{"winner":"a","score_a":80,"score_b":60,"critique":"ab"}"#,
            r#"{"winner":"a","score_a":80,"score_b":60,"critique":"ab-swap"}"#,
            // AC: same positional collapse.
            r#"{"winner":"a","score_a":75,"score_b":55,"critique":"ac"}"#,
            r#"{"winner":"a","score_a":75,"score_b":55,"critique":"ac-swap"}"#,
            // BC: decisive — B wins both passes by 30.
            r#"{"winner":"a","score_a":90,"score_b":60,"critique":"bc"}"#,
            r#"{"winner":"b","score_a":60,"score_b":90,"critique":"bc-swap"}"#,
        ]);

        let verdict = assay3(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("DIFF-A", "ok"),
            &cast("DIFF-B", "ok"),
            &cast("DIFF-C", "ok"),
            None,
        )
        .await
        .expect("verdict");

        assert!(verdict.tie, "seat-order coin flips must not rank the round");
        assert_eq!(verdict.margin, 0, "a tie never early-stops the duel");
    }

    #[tokio::test]
    async fn three_way_cycle_is_a_tie_with_margin_zero() {
        // A beats B, C beats A, B beats C: one pairwise win each.
        let judge = MockJudge::new(&[
            r#"{"winner":"a","score_a":80,"score_b":70,"critique":"ab"}"#,
            r#"{"winner":"b","score_a":70,"score_b":80,"critique":"ab-swap"}"#,
            r#"{"winner":"b","score_a":60,"score_b":80,"critique":"ac"}"#,
            r#"{"winner":"a","score_a":80,"score_b":60,"critique":"ac-swap"}"#,
            r#"{"winner":"a","score_a":75,"score_b":65,"critique":"bc"}"#,
            r#"{"winner":"b","score_a":65,"score_b":75,"critique":"bc-swap"}"#,
        ]);

        let verdict = assay3(
            &judge,
            "openai/gpt-5",
            "add retry",
            &cast("DIFF-A", "ok"),
            &cast("DIFF-B", "ok"),
            &cast("DIFF-C", "ok"),
            None,
        )
        .await
        .expect("verdict");

        assert!(verdict.tie, "cycle must not produce a ranking");
        assert_eq!(verdict.winner, 'a', "tie defaults to cast a");
        assert_eq!(verdict.margin, 0, "tie margin never early-stops");
    }

    #[tokio::test]
    async fn no_tools_preamble_leads_the_system_prompt_and_trailer_ends_the_rubric() {
        let judge = MockJudge::new(&[
            r#"{"winner":"a","score_a":80,"score_b":70,"critique":"c1"}"#,
            r#"{"winner":"b","score_a":70,"score_b":80,"critique":"c2"}"#,
        ]);

        assay(
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

        for req in judge.requests() {
            assert!(req.tools.is_empty(), "judge never carries tools");
            assert!(
                req.messages[0].content.starts_with(NO_TOOLS_PREAMBLE),
                "consequence-first preamble leads: {}",
                req.messages[0].content
            );
            assert!(
                req.messages[1].content.trim_end().ends_with(NO_TOOLS_TRAILER),
                "matching trailer ends the rubric"
            );
        }
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

    #[tokio::test]
    async fn rule_blocks_on_a_block_decision() {
        let judge = MockJudge::new(&[
            r#"{"decision":"block","reason":"writes outside the scope"}"#,
        ]);
        let ruling = rule(&judge, "openai/gpt-5", "refuse scope escapes", "{}")
            .await
            .expect("ruling");
        assert!(ruling.block);
        assert_eq!(ruling.reason, "writes outside the scope");
    }

    #[tokio::test]
    async fn rule_allows_and_carries_the_reason() {
        let judge = MockJudge::new(&[r#"{"decision":"allow","reason":"in scope"}"#]);
        let ruling = rule(&judge, "openai/gpt-5", "gate", "{}")
            .await
            .expect("ruling");
        assert!(!ruling.block);
        assert_eq!(ruling.reason, "in scope");
    }

    #[tokio::test]
    async fn rule_retries_once_on_malformed_json() {
        let judge = MockJudge::new(&[
            "I think this is fine, honestly.",
            r#"{"decision":"allow","reason":"second try parsed"}"#,
        ]);
        let ruling = rule(&judge, "openai/gpt-5", "gate", "{}")
            .await
            .expect("ruling");
        assert!(!ruling.block);
        assert_eq!(ruling.reason, "second try parsed");
        let reqs = judge.requests();
        assert_eq!(reqs.len(), 2, "one retry, not more");
        // The nudge rides on the second call, after the bad reply.
        let second = &reqs[1].messages;
        assert!(second.last().unwrap().content.contains(RETRY_NUDGE));
    }

    #[tokio::test]
    async fn rule_errors_after_a_second_malformed_reply() {
        let judge = MockJudge::new(&["not json", "still not json"]);
        let err = rule(&judge, "openai/gpt-5", "gate", "{}")
            .await
            .expect_err("two malformed replies must not fabricate an allow");
        assert!(err.to_string().contains("malformed"));
    }

    #[tokio::test]
    async fn rule_refuses_an_unrecognized_decision() {
        // "maybe" is not allow and not block. Silently allowing would turn a
        // confused model into an open gate.
        let judge = MockJudge::new(&[
            r#"{"decision":"maybe","reason":"unsure"}"#,
            r#"{"decision":"maybe","reason":"still unsure"}"#,
        ]);
        assert!(rule(&judge, "openai/gpt-5", "gate", "{}").await.is_err());
    }

    #[tokio::test]
    async fn rule_extracts_an_object_wrapped_in_prose() {
        let judge = MockJudge::new(&[
            "Sure! ```json\n{\"decision\":\"block\",\"reason\":\"rm -rf\"}\n``` hope that helps",
        ]);
        let ruling = rule(&judge, "openai/gpt-5", "gate", "{}")
            .await
            .expect("ruling");
        assert!(ruling.block);
        assert_eq!(ruling.reason, "rm -rf");
    }

    #[tokio::test]
    async fn rule_frames_the_call_as_no_tools_and_carries_the_payload() {
        let judge = MockJudge::new(&[r#"{"decision":"allow","reason":""}"#]);
        rule(
            &judge,
            "openai/gpt-5",
            "block anything touching secrets",
            r#"{"tool":"bash","command":"cat .env"}"#,
        )
        .await
        .expect("ruling");

        let req = &judge.requests()[0];
        assert!(req.tools.is_empty(), "a gate never calls tools");
        assert_eq!(req.role, Role::Judge);
        assert!(req.messages[0].content.starts_with(NO_TOOLS_PREAMBLE));
        let user = &req.messages[1].content;
        assert!(user.contains("block anything touching secrets"));
        assert!(user.contains("cat .env"), "the event payload reaches the model");
        assert!(user.trim_end().ends_with(NO_TOOLS_TRAILER));
    }
}
