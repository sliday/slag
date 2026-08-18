//! compact — v2 context management: cheap no-LLM pruning passes.
//!
//! Stage one stubs old *replayable* tool results down to a one-line marker
//! (re-runnable reads lose nothing the model cannot recover). Stage two,
//! when stubbing alone cannot fit the budget, drops whole API rounds of old
//! history — an assistant tool_calls message together with all its tool
//! results — because OpenAI-format providers reject a tool message whose
//! originating call is gone. System prompt, the original task, and the last
//! 6 messages are never touched (hermes two-stage compaction).

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use serde_json::Value;

use super::agent::{is_context_overflow, parse_overflow_tokens, CHARS_PER_TOKEN};
use super::{ChatMessage, ChatRequest, Provider};
use crate::error::SlagError;

const PRUNABLE_MIN_CHARS: usize = 500;
const KEEP_TAIL: usize = 6;
const STUB_HEAD_CHARS: usize = 120;
const STUB_PREFIX: &str = "[pruned old tool result: ";
/// Summarizer overflow retries: drop oldest rounds and re-ask, then fail.
const MAX_SUMMARY_RETRIES: usize = 3;
/// When the overflow 400 carries no "A + B > C" numbers, drop this
/// fraction of the summarizer's input per retry (1/5 = 20%).
const SUMMARY_DROP_FALLBACK_DIV: usize = 5;
/// Extra tokens dropped past a parsed overflow gap so the retry clears it.
const SUMMARY_GAP_SLACK_TOKENS: u64 = 512;

/// Consequence-first no-tools preamble for text-only LLM calls (the judge
/// and the compact summarizer). Claude Code measured the tool-call
/// fallback rate dropping 2.79% -> 0.01% with this framing.
pub(crate) const NO_TOOLS_PREAMBLE: &str =
    "Respond with TEXT/JSON ONLY — tool calls will be REJECTED and waste your only turn.";
/// Matching trailer for the end of the prompt.
pub(crate) const NO_TOOLS_TRAILER: &str =
    "Reminder: respond with TEXT/JSON ONLY — any tool call will be REJECTED and waste your \
only turn.";

/// Rust port of Claude Code's BASE_COMPACT_PROMPT: a 9-section summary
/// template with an <analysis> scratchpad (stripped before reuse).
const BASE_COMPACT_PROMPT: &str = "Your task is to create a detailed summary of the \
conversation so far, paying close attention to the original task and the work already done. \
The summary must capture the technical details, code patterns, and decisions essential for \
continuing the work without losing context.

Before providing your final summary, wrap your analysis in <analysis> tags to organize your \
thoughts: chronologically review the conversation, identifying each request, the approach \
taken, key decisions, and code changes.

Your summary must include exactly these 9 sections:
1. Primary Request and Intent: all explicit requests and intents, in detail
2. Key Technical Concepts: technologies, frameworks, and conventions in play
3. Files and Code Sections: files examined, modified, or created, with the important snippets
4. Errors and Fixes: errors hit and how they were fixed, including any feedback received
5. Problem Solving: problems solved and any ongoing troubleshooting
6. All User Messages: every non-tool user message, preserving the exact asks
7. Pending Tasks: tasks explicitly asked for that are not yet done
8. Current Work: precisely what was being worked on immediately before this summary
9. Optional Next Step: the single next step that directly continues the current work, or \
nothing if the last task was completed";

/// Item 44: the summary is delivered as a resume-silently continuation so
/// the model picks the task back up instead of acknowledging the break.
const CONTINUATION_PREFIX: &str = "This session is continued from a previous conversation \
that ran out of context. The summary below covers the earlier conversation. Pick up the \
last task exactly where it left off, as if the break never happened — do not acknowledge \
this summary or the interruption. Exact earlier output, if ever needed, is preserved in \
slag's JSONL event log.";

/// Total conversation chars: message content plus assistant tool-call
/// arguments — both count against the provider's context window.
pub fn convo_chars(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            let args: usize = m
                .tool_calls
                .as_ref()
                .map(|tcs| tcs.iter().map(|t| t.arguments.chars().count()).sum())
                .unwrap_or(0);
            m.content.chars().count() + args
        })
        .sum()
}

/// Shrink the conversation until total content chars fit the budget or
/// nothing prunable remains. Returns true when anything changed, so callers
/// tracking token anchors know their estimates went stale. Never touches
/// the system message, the first user message, or the last `KEEP_TAIL`
/// messages; drops always take whole rounds.
pub fn compact(messages: &mut Vec<ChatMessage>, char_budget: usize) -> bool {
    let mut changed = false;
    while convo_chars(messages) > char_budget {
        // Stage 1: stub the oldest replayable tool result.
        if let Some(idx) = stub_candidate(messages) {
            messages[idx].content = stub(&messages[idx].content);
            changed = true;
            continue;
        }
        // Stage 2: drop the oldest whole tool round.
        let Some(round) = drop_candidate(messages) else {
            return changed; // nothing prunable — stop, never loop forever
        };
        messages.drain(round);
        changed = true;
    }
    changed
}

/// Stage-three compaction (item 42): when stub-pruning cannot reach the
/// budget, ask the provider for a 9-section summary of the old history,
/// replace the head with one user message carrying it (as a resume-silently
/// continuation), and keep the recent tail verbatim. `file_context` is the
/// most-recently-touched files' current content; entries whose full read
/// still survives (unstubbed) in the tail are skipped, the rest ride a
/// system-reminder after the summary. Returns Ok(false) when there is
/// nothing beyond system, task, and the protected tail to summarize.
pub async fn summarize(
    provider: &dyn Provider,
    model: &str,
    messages: &mut Vec<ChatMessage>,
    file_context: &[(String, String)],
) -> Result<bool, SlagError> {
    let Some(cut) = summary_cut(messages) else {
        return Ok(false);
    };
    // Head skips the system message (index 0); it survives verbatim.
    let mut head: Vec<ChatMessage> = messages[1..cut].to_vec();
    let tail: Vec<ChatMessage> = messages[cut..].to_vec();

    // Item 45: an overflowing summary call retries with the oldest rounds
    // dropped — sized to the parsed gap, else 20% — at most 3 times.
    let mut retries = 0usize;
    let summary = loop {
        match provider.chat(summary_request(model, &head)).await {
            Ok(resp) => break strip_analysis(&resp.content),
            Err(e) if is_context_overflow(&e) && retries < MAX_SUMMARY_RETRIES => {
                retries += 1;
                let need = overflow_gap_chars(&e)
                    .unwrap_or_else(|| convo_chars(&head) / SUMMARY_DROP_FALLBACK_DIV)
                    .max(1);
                if !drop_oldest_rounds(&mut head, need) {
                    return Err(e); // nothing droppable — retrying is futile
                }
            }
            Err(e) => return Err(e),
        }
    };
    if summary.trim().is_empty() {
        return Ok(false); // a blank summary would erase history for nothing
    }

    let mut body = format!("{CONTINUATION_PREFIX}\n\n{summary}");
    let reinject = files_not_in_tail(file_context, &tail);
    if !reinject.is_empty() {
        body.push_str(
            "\n\n<system-reminder>\nCurrent content of the files most recently worked on, \
re-read after compaction:\n",
        );
        for (path, content) in &reinject {
            body.push_str(&format!("\n## {path}\n{content}\n"));
        }
        body.push_str("</system-reminder>");
    }

    let mut replacement = Vec::with_capacity(2 + tail.len());
    replacement.push(messages[0].clone());
    replacement.push(ChatMessage::user(body));
    replacement.extend(tail);
    *messages = replacement;
    Ok(true)
}

/// Round-aligned boundary: everything before it (bar the system message)
/// gets summarized, the tail after it is kept verbatim — never splitting
/// an assistant tool_calls round from its results. None when nothing
/// beyond system, task, and the protected tail would be summarized.
fn summary_cut(messages: &[ChatMessage]) -> Option<usize> {
    let target = messages.len().saturating_sub(KEEP_TAIL);
    let cut = group_rounds(messages)
        .into_iter()
        .find(|r| r.end > target)
        .map(|r| r.start)?;
    (cut > 2).then_some(cut)
}

/// The no-tools summarization request (item 43): tools empty AND the
/// consequence-first preamble leads, with a matching trailer.
fn summary_request(model: &str, head: &[ChatMessage]) -> ChatRequest {
    let prompt = format!(
        "{BASE_COMPACT_PROMPT}\n\n## Conversation to summarize\n{}\n{NO_TOOLS_TRAILER}",
        render_transcript(head)
    );
    ChatRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage::system(format!(
                "{NO_TOOLS_PREAMBLE} You summarize an agent's working conversation so the \
agent can continue in a fresh context."
            )),
            ChatMessage::user(prompt),
        ],
        tools: vec![],
        effort: None,
        max_tokens: None,
    }
}

/// Flatten messages into a role-labelled transcript for the summarizer;
/// assistant tool calls render inline so the summary can name them.
fn render_transcript(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for m in messages {
        match m.role.as_str() {
            "tool" => out.push_str(&format!("tool result: {}", m.content)),
            role => {
                out.push_str(&format!("{role}: {}", m.content));
                if let Some(calls) = &m.tool_calls {
                    for c in calls {
                        out.push_str(&format!("\n[called {}({})]", c.name, c.arguments));
                    }
                }
            }
        }
        out.push_str("\n\n");
    }
    out
}

/// Chars to drop so a retry clears a parsed "input + output > limit" gap.
fn overflow_gap_chars(e: &SlagError) -> Option<usize> {
    let (input, output, limit) = parse_overflow_tokens(&e.to_string())?;
    let over = (input + output).checked_sub(limit)?;
    Some(((over + SUMMARY_GAP_SLACK_TOKENS) as usize) * CHARS_PER_TOKEN)
}

/// Drop whole rounds from the front of the summarizer's input — never the
/// original task at index 0 — until at least `need_chars` are gone or
/// nothing droppable remains. Returns whether anything was dropped.
fn drop_oldest_rounds(head: &mut Vec<ChatMessage>, need_chars: usize) -> bool {
    let mut dropped = 0usize;
    let mut changed = false;
    while dropped < need_chars && head.len() > 1 {
        let Some(round) = group_rounds(head).into_iter().find(|r| r.start >= 1) else {
            break;
        };
        dropped += convo_chars(&head[round.clone()]);
        head.drain(round);
        changed = true;
    }
    changed
}

/// Remove the <analysis>…</analysis> scratchpad before the summary is
/// reused as context.
fn strip_analysis(text: &str) -> String {
    const OPEN: &str = "<analysis>";
    const CLOSE: &str = "</analysis>";
    let mut out = text.to_string();
    while let (Some(s), Some(e)) = (out.find(OPEN), out.find(CLOSE)) {
        if e < s {
            break;
        }
        out.replace_range(s..e + CLOSE.len(), "");
    }
    out.trim().to_string()
}

/// Filter re-injection candidates down to files whose full read does NOT
/// survive in the kept tail — a live (unstubbed) tail read already has
/// the content in context.
fn files_not_in_tail<'a>(
    file_context: &'a [(String, String)],
    tail: &[ChatMessage],
) -> Vec<&'a (String, String)> {
    let meta = result_meta(tail);
    let live: Vec<&str> = tail
        .iter()
        .zip(&meta)
        .filter_map(|(m, meta)| match meta {
            Some((name, Some(path)))
                if name == "read_file"
                    && !m.content.starts_with(STUB_PREFIX)
                    && !m.content.starts_with("[unchanged") =>
            {
                Some(path.as_str())
            }
            _ => None,
        })
        .collect();
    file_context
        .iter()
        .filter(|(path, _)| !live.iter().any(|p| same_file(p, path)))
        .collect()
}

/// Path equality tolerant of one side being absolute and the other
/// workspace-relative; two relative paths must match exactly.
fn same_file(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    (a.starts_with('/') && a.ends_with(&format!("/{b}")))
        || (b.starts_with('/') && b.ends_with(&format!("/{a}")))
}

/// Group messages into API rounds: an assistant message carrying tool_calls
/// opens a round and the tool results that follow belong to it; every other
/// message is a round of its own.
fn group_rounds(messages: &[ChatMessage]) -> Vec<Range<usize>> {
    let mut rounds = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let start = i;
        i += 1;
        if opens_tool_round(&messages[start]) {
            while i < messages.len() && messages[i].role == "tool" {
                i += 1;
            }
        }
        rounds.push(start..i);
    }
    rounds
}

fn opens_tool_round(m: &ChatMessage) -> bool {
    m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
}

/// Per-message (tool name, path argument) for tool results, resolved from
/// the owning round's assistant tool_calls. `None` for non-tool messages
/// and for results whose call cannot be found.
fn result_meta(messages: &[ChatMessage]) -> Vec<Option<(String, Option<String>)>> {
    let mut meta: Vec<Option<(String, Option<String>)>> = vec![None; messages.len()];
    for round in group_rounds(messages) {
        let head = &messages[round.start];
        let Some(calls) = head.tool_calls.as_ref().filter(|_| opens_tool_round(head)) else {
            continue;
        };
        for i in round.start + 1..round.end {
            let call = messages[i]
                .tool_call_id
                .as_deref()
                .and_then(|id| calls.iter().find(|c| c.id == id));
            meta[i] = call.map(|c| (c.name.clone(), path_arg(&c.arguments)));
        }
    }
    meta
}

fn path_arg(arguments: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    Some(args.get("path")?.as_str()?.to_string())
}

/// Only replayable read-only results are worth stubbing: re-running the
/// tool recovers the content. Edit and write confirmations and the finish
/// summary stay. Unresolved results stay eligible — a stub keeps the
/// message (and its tool_call_id) in place, so pairing cannot break.
fn stub_eligible(meta: Option<&(String, Option<String>)>) -> bool {
    match meta {
        Some((name, _)) => matches!(
            name.as_str(),
            "read_file" | "grep" | "glob" | "bash" | "recipe_view"
        ),
        None => true,
    }
}

/// Oldest tool result safe to stub, or None. Skips the tail, the newest
/// tool result overall (keepRecent floors at 1 — the model never loses all
/// working context), non-replayable results, and the newest result per
/// file path (the model's current view of that file).
fn stub_candidate(messages: &[ChatMessage]) -> Option<usize> {
    let cutoff = messages.len().saturating_sub(KEEP_TAIL);
    let last_tool = messages.iter().rposition(|m| m.role == "tool");
    let meta = result_meta(messages);

    // Newest result per file path stays readable when older duplicates
    // exist — stale reads prune first, the model's current view survives.
    // Solitary results follow the ordinary age rules.
    let mut seen: HashMap<&str, (usize, usize)> = HashMap::new(); // path -> (count, newest)
    for (i, m) in meta.iter().enumerate() {
        if let Some((_, Some(path))) = m {
            let entry = seen.entry(path.as_str()).or_insert((0, i));
            entry.0 += 1;
            entry.1 = i;
        }
    }
    let keep_per_path: HashSet<usize> = seen
        .into_values()
        .filter(|&(count, _)| count > 1)
        .map(|(_, newest)| newest)
        .collect();

    messages.iter().enumerate().position(|(i, m)| {
        m.role == "tool"
            && i < cutoff
            && Some(i) != last_tool
            && prunable(&m.content)
            && stub_eligible(meta[i].as_ref())
            && !keep_per_path.contains(&i)
    })
}

/// Oldest whole tool round that can be dropped, or None. The system
/// message and original task (rounds starting before index 2) and any
/// round reaching into the tail are protected — a round is dropped in
/// full or not at all.
fn drop_candidate(messages: &[ChatMessage]) -> Option<Range<usize>> {
    let cutoff = messages.len().saturating_sub(KEEP_TAIL);
    group_rounds(messages)
        .into_iter()
        .find(|r| r.start >= 2 && r.end <= cutoff && opens_tool_round(&messages[r.start]))
}

fn prunable(content: &str) -> bool {
    content.chars().count() > PRUNABLE_MIN_CHARS && !content.starts_with(STUB_PREFIX)
}

fn stub(content: &str) -> String {
    let head: String = content.chars().take(STUB_HEAD_CHARS).collect();
    format!("{STUB_PREFIX}{head}...]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ToolCall;

    fn long_tool(id: &str, len: usize) -> ChatMessage {
        ChatMessage::tool_result(id, "x".repeat(len))
    }

    fn base_convo() -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("s".repeat(100)),
            ChatMessage::user("task ".repeat(20)),
        ]
    }

    fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    /// One full API round: assistant tool_calls + its tool result.
    fn round(id: &str, name: &str, args: serde_json::Value, result_len: usize) -> Vec<ChatMessage> {
        vec![
            ChatMessage::assistant("", Some(vec![call(id, name, args)])),
            long_tool(id, result_len),
        ]
    }

    fn pad_tail(messages: &mut Vec<ChatMessage>) {
        for i in 0..KEEP_TAIL {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }
    }

    /// Every tool message must have an assistant tool_call with its id
    /// somewhere before it — the invariant OpenAI-format backends enforce.
    fn assert_no_orphans(messages: &[ChatMessage]) {
        for (i, m) in messages.iter().enumerate() {
            if m.role != "tool" {
                continue;
            }
            let id = m.tool_call_id.as_deref().expect("tool result carries id");
            let paired = messages[..i].iter().any(|a| {
                a.tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|c| c.id == id))
            });
            assert!(paired, "orphan tool result at index {i} (id {id})");
        }
    }

    #[test]
    fn prunes_oldest_long_tool_result_first() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 2000));
        messages.push(long_tool("call_2", 2000));
        messages.push(long_tool("call_3", 2000));
        // 6-message tail guard: pad so the tool results sit outside it.
        pad_tail(&mut messages);

        compact(&mut messages, 500);

        assert!(messages[2].content.starts_with(STUB_PREFIX));
        assert!(messages[2].content.ends_with("...]"));
        // Still over budget after the first prune, so call_2 went too.
        assert!(messages[3].content.starts_with(STUB_PREFIX));
        // keepRecent floors at 1: the newest tool result always survives.
        assert_eq!(messages[4].content.chars().count(), 2000);
    }

    #[test]
    fn stops_pruning_once_under_budget() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 5000));
        messages.push(long_tool("call_2", 600));
        pad_tail(&mut messages);

        compact(&mut messages, 2000);

        assert!(messages[2].content.starts_with(STUB_PREFIX));
        // Budget satisfied after the first prune; call_2 untouched.
        assert_eq!(messages[3].content.chars().count(), 600);
    }

    #[test]
    fn never_touches_system_first_user_or_tail() {
        let mut messages = base_convo();
        // The only long tool result sits inside the last-6 tail.
        messages.push(long_tool("call_1", 2000));
        messages.push(ChatMessage::user("go"));

        let before: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
        let changed = compact(&mut messages, 10);
        let after: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();

        assert_eq!(before, after);
        assert!(!changed, "no-op must report unchanged");
    }

    #[test]
    fn short_tool_results_are_not_pruned() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 400)); // under the 500-char floor
        pad_tail(&mut messages);

        compact(&mut messages, 10);
        assert_eq!(messages[2].content.chars().count(), 400);
    }

    #[test]
    fn stub_keeps_first_120_chars_and_is_not_repruned() {
        let content: String = ('a'..='z').cycle().take(1000).collect();
        let mut messages = base_convo();
        messages.push(ChatMessage::tool_result("call_1", content.clone()));
        // A newer short result keeps call_1 clear of the keepRecent floor.
        messages.push(ChatMessage::tool_result("call_2", "ok"));
        pad_tail(&mut messages);

        compact(&mut messages, 10);

        let head: String = content.chars().take(120).collect();
        assert_eq!(messages[2].content, format!("{STUB_PREFIX}{head}...]"));

        // Second pass with the stub in place must terminate untouched.
        let snapshot = messages[2].content.clone();
        compact(&mut messages, 10);
        assert_eq!(messages[2].content, snapshot);
    }

    #[test]
    fn under_budget_is_a_no_op() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 2000));
        let before = messages[2].content.clone();
        assert!(!compact(&mut messages, 1_000_000));
        assert_eq!(messages[2].content, before);
    }

    #[test]
    fn reports_change_when_anything_was_pruned() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 2000));
        messages.push(long_tool("call_2", 2000));
        pad_tail(&mut messages);
        assert!(compact(&mut messages, 500), "stub must report changed");
    }

    #[test]
    fn the_newest_tool_result_always_survives_stubbing() {
        let mut messages = base_convo();
        // Only one tool result, well outside the tail and over budget: the
        // keepRecent floor of 1 still protects it — the model never loses
        // all of its working context.
        messages.push(long_tool("call_1", 2000));
        pad_tail(&mut messages);

        assert!(!compact(&mut messages, 500));
        assert_eq!(messages[2].content.chars().count(), 2000);
    }

    #[test]
    fn group_rounds_binds_tool_results_to_their_assistant() {
        let mut messages = base_convo();
        messages.push(ChatMessage::assistant(
            "",
            Some(vec![
                call("a1", "read_file", serde_json::json!({"path": "x"})),
                call("a2", "read_file", serde_json::json!({"path": "y"})),
            ]),
        ));
        messages.push(long_tool("a1", 10));
        messages.push(long_tool("a2", 10));
        messages.push(ChatMessage::user("next"));
        messages.push(ChatMessage::assistant("plain text", None));

        let rounds = group_rounds(&messages);
        // system | task | assistant+2 results | user | plain assistant
        assert_eq!(rounds, vec![0..1, 1..2, 2..5, 5..6, 6..7]);
    }

    #[test]
    fn edit_results_are_never_stubbed() {
        let mut messages = base_convo();
        messages.extend(round(
            "r1",
            "read_file",
            serde_json::json!({"path": "a.rs"}),
            2000,
        ));
        messages.extend(round(
            "e1",
            "edit_file",
            serde_json::json!({"path": "b.rs", "old_string": "x", "new_string": "y"}),
            2000,
        ));
        pad_tail(&mut messages);

        // Budget reachable by stubbing the (older) read alone.
        compact(&mut messages, 3000);

        assert!(messages[3].content.starts_with(STUB_PREFIX), "read stubbed");
        assert_eq!(
            messages[5].content.chars().count(),
            2000,
            "edit result kept in full"
        );
    }

    #[test]
    fn newest_result_per_path_survives_stubbing() {
        let mut messages = base_convo();
        // Two reads of the same file, both outside the tail.
        messages.extend(round("r1", "read_file", serde_json::json!({"path": "a.rs"}), 2000));
        messages.extend(round("r2", "read_file", serde_json::json!({"path": "a.rs"}), 2000));
        pad_tail(&mut messages);

        compact(&mut messages, 3000);

        assert!(messages[3].content.starts_with(STUB_PREFIX), "stale read stubbed");
        assert_eq!(
            messages[5].content.chars().count(),
            2000,
            "newest read of a.rs stays readable"
        );
    }

    #[test]
    fn drops_whole_rounds_when_stubbing_cannot_fit() {
        let mut messages = base_convo();
        // Results under the 500-char stub floor: stage 1 has nothing to do,
        // stage 2 must drop rounds — assistant and results together.
        for i in 0..4 {
            messages.extend(round(
                &format!("c{i}"),
                "read_file",
                serde_json::json!({"path": format!("f{i}.rs")}),
                400,
            ));
        }
        pad_tail(&mut messages);

        let len_before = messages.len();
        assert!(compact(&mut messages, 600));

        assert!(messages.len() < len_before, "rounds were dropped");
        assert_no_orphans(&messages);
        // Protected head survives.
        assert_eq!(messages[0].role, "system");
        assert!(messages[1].content.starts_with("task"));
        // The tail user padding survives round-aligned dropping.
        assert!(messages.iter().filter(|m| m.role == "user").count() >= KEEP_TAIL);
    }

    #[test]
    fn a_round_reaching_into_the_tail_is_never_split() {
        let mut messages = base_convo();
        messages.extend(round("c0", "read_file", serde_json::json!({"path": "old.rs"}), 400));
        // This round's results land inside the last-6 tail: protected whole.
        messages.push(ChatMessage::assistant(
            "",
            Some(vec![
                call("t1", "read_file", serde_json::json!({"path": "x"})),
                call("t2", "read_file", serde_json::json!({"path": "y"})),
            ]),
        ));
        messages.push(long_tool("t1", 400));
        messages.push(long_tool("t2", 400));
        for i in 0..3 {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }

        compact(&mut messages, 10);

        assert_no_orphans(&messages);
        // The straddling round survives intact.
        let ids: Vec<Option<&str>> = messages.iter().map(|m| m.tool_call_id.as_deref()).collect();
        assert!(ids.contains(&Some("t1")) && ids.contains(&Some("t2")));
        // The old droppable round is gone.
        assert!(!ids.contains(&Some("c0")));
    }

    // ---- summarizer (stage three) ----

    use crate::engine::{FinishReason, NormalizedResponse, Usage};
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Scripted summarizer provider: pops one Result per chat call.
    struct MockSummarizer {
        script: Mutex<VecDeque<Result<String, String>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl MockSummarizer {
        fn new(script: Vec<Result<&str, &str>>) -> Self {
            Self {
                script: Mutex::new(
                    script
                        .into_iter()
                        .map(|r| r.map(str::to_string).map_err(str::to_string))
                        .collect(),
                ),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Provider for MockSummarizer {
        fn chat(
            &self,
            req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            self.requests.lock().unwrap().push(req);
            let next = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("summarizer script exhausted");
            Box::pin(async move {
                next.map(|content| NormalizedResponse {
                    model: None,
                    content,
                    tool_calls: vec![],
                    finish_reason: FinishReason::Stop,
                    reasoning: None,
                    reasoning_details: None,
                    usage: Usage::default(),
                })
                .map_err(SlagError::Provider)
            })
        }
    }

    /// base + 6 read rounds (f0..f5) + a 6-message user tail.
    fn summarizable_convo() -> Vec<ChatMessage> {
        let mut messages = base_convo();
        for i in 0..6 {
            messages.extend(round(
                &format!("c{i}"),
                "read_file",
                serde_json::json!({"path": format!("f{i}.rs")}),
                600,
            ));
        }
        pad_tail(&mut messages);
        messages
    }

    #[tokio::test]
    async fn summarizer_replaces_head_and_keeps_tail() {
        let provider =
            MockSummarizer::new(vec![Ok("<analysis>scratch notes</analysis>\nTHE-9-SECTIONS")]);
        let mut messages = summarizable_convo();
        let tail_before: Vec<String> =
            messages[messages.len() - KEEP_TAIL..].iter().map(|m| m.content.clone()).collect();

        let changed = summarize(&provider, "test/model", &mut messages, &[])
            .await
            .expect("summarize ok");
        assert!(changed);

        // system | continuation user | verbatim tail.
        assert_eq!(messages.len(), 2 + KEEP_TAIL);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        let body = &messages[1].content;
        assert!(body.contains("continued from a previous conversation"), "{body}");
        assert!(body.contains("as if the break never happened"), "{body}");
        assert!(body.contains("JSONL event log"), "{body}");
        assert!(body.contains("THE-9-SECTIONS"), "{body}");
        // The <analysis> scratchpad never reaches the continued context.
        assert!(!body.contains("analysis"), "{body}");
        assert!(!body.contains("scratch notes"), "{body}");
        let tail_after: Vec<String> =
            messages[2..].iter().map(|m| m.content.clone()).collect();
        assert_eq!(tail_before, tail_after);

        // The summary call: no tools, consequence-first preamble leading
        // the system message, template + trailer in the user prompt.
        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert!(req.tools.is_empty());
        assert!(req.messages[0].content.starts_with(NO_TOOLS_PREAMBLE));
        let prompt = &req.messages[1].content;
        assert!(prompt.contains("Primary Request and Intent"), "template present");
        assert!(prompt.contains("Optional Next Step"), "all 9 sections");
        assert!(prompt.contains("f0.rs"), "head transcript present");
        assert!(prompt.trim_end().ends_with(NO_TOOLS_TRAILER), "trailer");
    }

    #[tokio::test]
    async fn summarizer_is_a_noop_when_only_protected_messages_exist() {
        let provider = MockSummarizer::new(vec![]);
        let mut messages = base_convo();
        pad_tail(&mut messages);
        let before: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();

        let changed = summarize(&provider, "test/model", &mut messages, &[])
            .await
            .expect("noop ok");
        assert!(!changed);
        let after: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
        assert_eq!(before, after);
        assert!(provider.requests().is_empty(), "no provider call");
    }

    #[tokio::test]
    async fn overflowing_summary_call_drops_oldest_rounds_and_retries() {
        let provider = MockSummarizer::new(vec![
            Err("400: maximum context length exceeded"),
            Ok("SUMMARY"),
        ]);
        let mut messages = summarizable_convo();

        let changed = summarize(&provider, "test/model", &mut messages, &[])
            .await
            .expect("retry ok");
        assert!(changed);

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let first = &requests[0].messages[1].content;
        let retry = &requests[1].messages[1].content;
        // Oldest round dropped from the retry; newest survives; the
        // original task message is never dropped.
        assert!(first.contains("f0.rs"));
        assert!(!retry.contains("f0.rs"), "oldest round dropped");
        assert!(retry.contains("f5.rs"), "newest round kept");
        assert!(retry.contains("task"), "original task protected");
        assert!(retry.len() < first.len());
    }

    #[tokio::test]
    async fn summary_overflow_fails_after_three_drop_retries() {
        let overflow = || Err("400: maximum context length exceeded");
        let provider =
            MockSummarizer::new(vec![overflow(), overflow(), overflow(), overflow()]);
        let mut messages = summarizable_convo();
        let before = messages.len();

        let err = summarize(&provider, "test/model", &mut messages, &[])
            .await
            .expect_err("must fail after 3 retries");
        assert!(err.to_string().contains("context"), "got: {err}");
        assert_eq!(provider.requests().len(), 4, "initial call + 3 retries");
        assert_eq!(messages.len(), before, "failed summarize leaves history intact");
    }

    #[tokio::test]
    async fn reinjected_files_skip_reads_surviving_in_the_tail() {
        let provider = MockSummarizer::new(vec![Ok("SUMMARY")]);
        // Head: 4 rounds. Tail: a LIVE read of a.rs plus 4 user turns.
        let mut messages = base_convo();
        for i in 0..4 {
            messages.extend(round(
                &format!("c{i}"),
                "read_file",
                serde_json::json!({"path": format!("f{i}.rs")}),
                600,
            ));
        }
        messages.extend(round("ra", "read_file", serde_json::json!({"path": "a.rs"}), 600));
        for i in 0..4 {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }

        let file_context = vec![
            ("a.rs".to_string(), "AAA-CONTENT".to_string()),
            ("b.rs".to_string(), "BBB-CONTENT".to_string()),
        ];
        let changed = summarize(&provider, "test/model", &mut messages, &file_context)
            .await
            .expect("summarize ok");
        assert!(changed);

        let body = &messages[1].content;
        assert!(body.contains("<system-reminder>"), "{body}");
        assert!(body.contains("## b.rs"), "{body}");
        assert!(body.contains("BBB-CONTENT"), "{body}");
        // a.rs's read survives live in the tail — not re-injected.
        assert!(!body.contains("## a.rs"), "{body}");
        assert!(!body.contains("AAA-CONTENT"), "{body}");
        // The live tail read itself survived.
        assert!(messages.iter().any(|m| m.tool_call_id.as_deref() == Some("ra")));
    }

    #[tokio::test]
    async fn stubbed_tail_reads_do_not_block_reinjection() {
        let provider = MockSummarizer::new(vec![Ok("SUMMARY")]);
        let mut messages = base_convo();
        for i in 0..4 {
            messages.extend(round(
                &format!("c{i}"),
                "read_file",
                serde_json::json!({"path": format!("f{i}.rs")}),
                600,
            ));
        }
        // Tail read of a.rs already stubbed by stage-one pruning: its
        // content is gone from context, so re-injection must proceed.
        messages.push(ChatMessage::assistant(
            "",
            Some(vec![call("ra", "read_file", serde_json::json!({"path": "a.rs"}))]),
        ));
        messages.push(ChatMessage::tool_result("ra", format!("{STUB_PREFIX}1|old...]")));
        for i in 0..4 {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }

        let file_context = vec![("a.rs".to_string(), "AAA-CONTENT".to_string())];
        summarize(&provider, "test/model", &mut messages, &file_context)
            .await
            .expect("summarize ok");
        let body = &messages[1].content;
        assert!(body.contains("## a.rs"), "{body}");
        assert!(body.contains("AAA-CONTENT"), "{body}");
    }

    #[test]
    fn strip_analysis_removes_the_scratchpad() {
        assert_eq!(
            strip_analysis("<analysis>thinking...</analysis>\nSummary body"),
            "Summary body"
        );
        assert_eq!(strip_analysis("no scratchpad here"), "no scratchpad here");
        // Unclosed tag: left alone rather than eating the summary.
        assert_eq!(strip_analysis("<analysis>oops no close"), "<analysis>oops no close");
        // Multiple spans all go.
        assert_eq!(
            strip_analysis("<analysis>a</analysis>X<analysis>b</analysis>Y"),
            "XY"
        );
    }

    #[test]
    fn overflow_gap_chars_sizes_the_drop_from_parsed_numbers() {
        let e = SlagError::Provider(
            "input length and max_tokens exceed context limit: 1100 + 100 > 1000".into(),
        );
        // (1100 + 100 - 1000 + 512 slack) tokens * 4 chars.
        assert_eq!(overflow_gap_chars(&e), Some((200 + 512) * 4));
        // No numbers → None → caller falls back to the 20% drop.
        let plain = SlagError::Provider("400: maximum context length exceeded".into());
        assert_eq!(overflow_gap_chars(&plain), None);
    }

    #[test]
    fn same_file_tolerates_relative_vs_absolute() {
        assert!(same_file("src/a.rs", "src/a.rs"));
        assert!(same_file("/root/ws/src/a.rs", "src/a.rs"));
        assert!(same_file("src/a.rs", "/root/ws/src/a.rs"));
        assert!(!same_file("b/a.rs", "a.rs"), "component boundary respected");
        assert!(!same_file("src/a.rs", "src/b.rs"));
    }
}
