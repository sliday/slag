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

use super::ChatMessage;

const PRUNABLE_MIN_CHARS: usize = 500;
const KEEP_TAIL: usize = 6;
const STUB_HEAD_CHARS: usize = 120;
const STUB_PREFIX: &str = "[pruned old tool result: ";

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
}
