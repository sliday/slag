//! compact — v1 context management: cheap no-LLM pruning pass.
//!
//! When the conversation grows past the char budget, old tool results get
//! stubbed down to a one-line marker. System prompt, the original task, and
//! the last 6 messages are never touched (hermes two-stage compaction,
//! stage one only for v1).

use super::ChatMessage;

const PRUNABLE_MIN_CHARS: usize = 500;
const KEEP_TAIL: usize = 6;
const STUB_HEAD_CHARS: usize = 120;
const STUB_PREFIX: &str = "[pruned old tool result: ";

/// Prune oldest oversized tool results until total content chars fit the
/// budget or nothing prunable remains. Never touches the system message,
/// the first user message, or the last `KEEP_TAIL` messages.
pub fn compact(messages: &mut Vec<ChatMessage>, char_budget: usize) {
    loop {
        let total: usize = messages
            .iter()
            .map(|m| {
                let args: usize = m
                    .tool_calls
                    .as_ref()
                    .map(|tcs| tcs.iter().map(|t| t.arguments.chars().count()).sum())
                    .unwrap_or(0);
                m.content.chars().count() + args
            })
            .sum();
        if total <= char_budget {
            return;
        }

        let cutoff = messages.len().saturating_sub(KEEP_TAIL);
        let candidate = messages
            .iter()
            .position(|m| m.role == "tool" && prunable(&m.content))
            .filter(|&i| i < cutoff);

        let Some(idx) = candidate else {
            return; // nothing prunable — stop, never loop forever
        };

        messages[idx].content = stub(&messages[idx].content);
    }
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

    fn long_tool(id: &str, len: usize) -> ChatMessage {
        ChatMessage::tool_result(id, "x".repeat(len))
    }

    fn base_convo() -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("s".repeat(100)),
            ChatMessage::user("task ".repeat(20)),
        ]
    }

    #[test]
    fn prunes_oldest_long_tool_result_first() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 2000));
        messages.push(long_tool("call_2", 2000));
        // 6-message tail guard: pad so the tool results sit outside it.
        for i in 0..6 {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }

        compact(&mut messages, 500);

        assert!(messages[2].content.starts_with(STUB_PREFIX));
        assert!(messages[2].content.ends_with("...]"));
        // Still over budget after the first prune, so call_2 went too.
        assert!(messages[3].content.starts_with(STUB_PREFIX));
    }

    #[test]
    fn stops_pruning_once_under_budget() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 5000));
        messages.push(long_tool("call_2", 600));
        for i in 0..6 {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }

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
        compact(&mut messages, 10);
        let after: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();

        assert_eq!(before, after);
    }

    #[test]
    fn short_tool_results_are_not_pruned() {
        let mut messages = base_convo();
        messages.push(long_tool("call_1", 400)); // under the 500-char floor
        for i in 0..6 {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }

        compact(&mut messages, 10);
        assert_eq!(messages[2].content.chars().count(), 400);
    }

    #[test]
    fn stub_keeps_first_120_chars_and_is_not_repruned() {
        let content: String = ('a'..='z').cycle().take(1000).collect();
        let mut messages = base_convo();
        messages.push(ChatMessage::tool_result("call_1", content.clone()));
        for i in 0..6 {
            messages.push(ChatMessage::user(format!("turn {i}")));
        }

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
        compact(&mut messages, 1_000_000);
        assert_eq!(messages[2].content, before);
    }
}
