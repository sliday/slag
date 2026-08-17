//! agent — the agentic loop, the smith brain.
//!
//! Drives provider turns and tool dispatch until the model calls `finish`,
//! stops emitting tool calls, or the turn budget runs out. Tool batches run
//! in reader/writer segments (hermes pattern #6): consecutive read-only
//! calls execute concurrently, any writer or unclassified call serializes.
//! Local tool bugs (including panics) become `is_error` tool results;
//! only provider errors propagate — ingot heat handles those retries.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::task::JoinHandle;

use super::compact::compact;
use super::events::preview;
use super::tools::ToolBox;
use super::{
    emit, CancelFlag, ChatMessage, ChatRequest, Effort, EngineEvent, EventTx, FinishReason,
    NormalizedResponse, Provider, SteerQueue, ToolCall, ToolOutcome,
};
use crate::error::SlagError;

const DEFAULT_MAX_TURNS: usize = 40;
const CHAR_BUDGET: usize = 600_000;
/// Overflow-shrink floor: below this, compaction cannot help — the
/// system prompt, task, and protected tail alone exceed the window.
const CHAR_BUDGET_FLOOR: usize = 16_000;
const PREVIEW_LEN: usize = 80;
const STEER_TAG: &str = "[STEER — operator message, follow it]";

/// Char budget for compaction. `SLAG_CHAR_BUDGET` overrides the default so
/// smaller-context models compact before the provider rejects the request.
fn char_budget_from_env() -> usize {
    std::env::var("SLAG_CHAR_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(CHAR_BUDGET)
}

/// Provider rejection caused by the request exceeding the model's context
/// window (OpenRouter surfaces these as 400s with varying phrasings).
fn is_context_overflow(e: &SlagError) -> bool {
    let s = e.to_string().to_lowercase();
    [
        "context length",
        "context_length",
        "context window",
        "maximum context",
        "too many tokens",
        "input is too long",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

fn convo_chars(messages: &[ChatMessage]) -> usize {
    messages.iter().map(|m| m.content.chars().count()).sum()
}

/// One smith session: a provider, a toolbox, a model, and a turn budget.
pub struct ForgeAgent {
    provider: Arc<dyn Provider>,
    toolbox: ToolBox,
    model: String,
    effort: Option<Effort>,
    max_turns: usize,
    char_budget: usize,
    events: Option<EventTx>,
    steer: Option<SteerQueue>,
    cancel: Option<CancelFlag>,
}

impl ForgeAgent {
    pub fn new(provider: Arc<dyn Provider>, toolbox: ToolBox, model: impl Into<String>) -> Self {
        Self {
            provider,
            toolbox,
            model: model.into(),
            effort: None,
            max_turns: DEFAULT_MAX_TURNS,
            char_budget: char_budget_from_env(),
            events: None,
            steer: None,
            cancel: None,
        }
    }

    #[cfg(test)]
    fn with_char_budget(mut self, chars: usize) -> Self {
        self.char_budget = chars.max(1);
        self
    }

    pub fn with_effort(mut self, effort: Option<Effort>) -> Self {
        self.effort = effort;
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns.max(1);
        self
    }

    pub fn with_events(mut self, tx: EventTx) -> Self {
        self.events = Some(tx);
        self
    }

    pub fn with_steer(mut self, q: SteerQueue) -> Self {
        self.steer = Some(q);
        self
    }

    pub fn with_cancel(mut self, f: CancelFlag) -> Self {
        self.cancel = Some(f);
        self
    }

    /// Run the loop to completion. Returns the final summary text.
    pub async fn run(&self, system: String, task: String) -> Result<String, SlagError> {
        // Steers drained into the conversation die with it if the provider
        // errors: re-queue them so the ingot's next heat re-delivers them.
        let mut applied_steers: Vec<String> = Vec::new();
        let result = self.run_inner(system, task, &mut applied_steers).await;
        if result.is_err() {
            self.requeue_steers(&applied_steers);
        }
        result
    }

    async fn run_inner(
        &self,
        system: String,
        task: String,
        applied_steers: &mut Vec<String>,
    ) -> Result<String, SlagError> {
        let mut messages = vec![ChatMessage::system(system), ChatMessage::user(task)];
        let mut continued = false;
        let mut char_budget = self.char_budget;

        for turn in 1..=self.max_turns {
            self.check_cancel()?;
            self.apply_steers(&mut messages, applied_steers);
            compact(&mut messages, char_budget);
            emit(&self.events, EngineEvent::TurnStart { turn });
            emit(&self.events, EngineEvent::ModelCall { model: self.model.clone() });

            let resp = match self.chat_shrinking(&mut messages, true, &mut char_budget).await {
                Ok(resp) => resp,
                Err(e) => {
                    emit(&self.events, EngineEvent::Error { message: e.to_string() });
                    return Err(e);
                }
            };
            // With a router in front, the requested id says nothing about
            // what actually did the work. Report the swap when it happens.
            if let Some(routed) = resp.model.as_deref().filter(|m| *m != self.model) {
                emit(
                    &self.events,
                    EngineEvent::ModelRouted {
                        requested: self.model.clone(),
                        routed: routed.to_string(),
                    },
                );
            }
            emit(&self.events, EngineEvent::Tokens { usage: resp.usage.clone() });

            if resp.tool_calls.is_empty() {
                if resp.finish_reason == FinishReason::Length && !continued {
                    // Truncated mid-thought: nudge once, then take what comes.
                    continued = true;
                    messages.push(ChatMessage::assistant(resp.content, None));
                    messages.push(ChatMessage::user("continue"));
                    continue;
                }
                emit(&self.events, EngineEvent::Finish { summary: resp.content.clone() });
                return Ok(resp.content);
            }

            let calls = resp.tool_calls;
            messages.push(
                ChatMessage::assistant(resp.content.clone(), Some(calls.clone()))
                    .with_reasoning_details(resp.reasoning_details.clone()),
            );

            let outcomes = self.dispatch_batch(&calls).await;

            // Every tool_call_id gets a tool result, errors included — the
            // API never sees a dangling id (hermes pattern #7).
            let mut finish_summary: Option<String> = None;
            for (call, outcome) in calls.iter().zip(&outcomes) {
                let content = if outcome.is_error {
                    format!("ERROR: {}", outcome.output)
                } else {
                    outcome.output.clone()
                };
                messages.push(ChatMessage::tool_result(&call.id, content));
                if call.name == "finish" && !outcome.is_error && finish_summary.is_none() {
                    finish_summary = Some(outcome.output.clone());
                }
            }

            if let Some(summary) = finish_summary {
                // Keep the same-turn assistant text: Plan mode delivers the
                // plan as final text before calling finish, and forge-mode
                // contracts (e.g. a CMD: line) may live there too.
                let result = if resp.content.trim().is_empty() {
                    summary
                } else {
                    format!("{}\n\n{}", resp.content, summary)
                };
                emit(&self.events, EngineEvent::Finish { summary: result.clone() });
                return Ok(result);
            }

            // Drain again right after tool execution: a steer typed during
            // a long bash lands before the next model call.
            self.apply_steers(&mut messages, applied_steers);
        }

        // Turn budget exhausted: one final no-tools call for a summary.
        messages.push(ChatMessage::user(
            "no more tool budget — summarize what was done",
        ));
        compact(&mut messages, char_budget);
        let resp = match self.chat_shrinking(&mut messages, false, &mut char_budget).await {
            Ok(resp) => resp,
            Err(e) => {
                emit(&self.events, EngineEvent::Error { message: e.to_string() });
                return Err(e);
            }
        };
        emit(&self.events, EngineEvent::Tokens { usage: resp.usage.clone() });
        emit(&self.events, EngineEvent::Finish { summary: resp.content.clone() });
        Ok(resp.content)
    }

    /// Hard interrupt: checked at each turn boundary, before the model call.
    fn check_cancel(&self) -> Result<(), SlagError> {
        let cancelled = self.cancel.as_ref().is_some_and(|f| f.load(Ordering::SeqCst));
        if cancelled {
            let e = SlagError::Cancelled;
            emit(&self.events, EngineEvent::Error { message: e.to_string() });
            return Err(e);
        }
        Ok(())
    }

    /// Chat with context-overflow recovery: on a context-window 400, halve
    /// the budget, compact, and retry — until the floor is reached or
    /// compaction stops making progress. Fixed budgets sized for large
    /// windows never fire compaction on smaller-context models otherwise.
    async fn chat_shrinking(
        &self,
        messages: &mut Vec<ChatMessage>,
        with_tools: bool,
        char_budget: &mut usize,
    ) -> Result<NormalizedResponse, SlagError> {
        loop {
            match self.provider.chat(self.request(messages.clone(), with_tools)).await {
                Ok(resp) => return Ok(resp),
                Err(e) if is_context_overflow(&e) && *char_budget > CHAR_BUDGET_FLOOR => {
                    *char_budget = (*char_budget / 2).max(CHAR_BUDGET_FLOOR);
                    let before = convo_chars(messages);
                    compact(messages, *char_budget);
                    if convo_chars(messages) == before {
                        return Err(e); // nothing prunable — retrying is futile
                    }
                    emit(
                        &self.events,
                        EngineEvent::Error {
                            message: format!(
                                "context overflow — compacted to {char_budget} chars, retrying"
                            ),
                        },
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Inject queued steering messages (hermes steer-into-tool-result
    /// pattern): each steer appends to the last tool result so the model
    /// reads it with the tool output; before any tool result exists it
    /// rides as a user message. Order is preserved. Every applied steer is
    /// recorded so a failed run can put it back in the queue.
    fn apply_steers(&self, messages: &mut Vec<ChatMessage>, applied: &mut Vec<String>) {
        for text in self.drain_steers() {
            emit(&self.events, EngineEvent::Steer { text: text.clone() });
            match messages.iter_mut().rev().find(|m| m.role == "tool") {
                Some(m) => {
                    m.content.push_str(&format!("\n\n{STEER_TAG}\n{text}"));
                }
                None => messages.push(ChatMessage::user(format!("{STEER_TAG}\n{text}"))),
            }
            applied.push(text);
        }
    }

    /// Put steers consumed by a failed run back at the front of the queue,
    /// ahead of anything queued since, so the retry heat re-delivers them.
    fn requeue_steers(&self, applied: &[String]) {
        if applied.is_empty() {
            return;
        }
        if let Some(q) = &self.steer {
            let mut q = q.lock().unwrap();
            let mut restored: Vec<String> = applied.to_vec();
            restored.append(&mut *q);
            *q = restored;
        }
    }

    /// Take all queued steers. Synchronous on purpose: the std Mutex guard
    /// is taken and dropped inside this fn, never held across an await.
    fn drain_steers(&self) -> Vec<String> {
        match &self.steer {
            Some(q) => std::mem::take(&mut *q.lock().unwrap()),
            None => Vec::new(),
        }
    }

    fn request(&self, messages: Vec<ChatMessage>, with_tools: bool) -> ChatRequest {
        ChatRequest {
            model: self.model.clone(),
            messages,
            tools: if with_tools { ToolBox::specs() } else { Vec::new() },
            effort: self.effort,
            max_tokens: None,
        }
    }

    /// Execute a tool batch in reader/writer segments, preserving model
    /// order in the returned outcomes.
    async fn dispatch_batch(&self, calls: &[ToolCall]) -> Vec<ToolOutcome> {
        let mut outcomes: Vec<Option<ToolOutcome>> = calls.iter().map(|_| None).collect();

        for segment in plan_segments(calls) {
            // Spawn the whole segment first (concurrent for readers), then
            // await in model order.
            let handles: Vec<(usize, JoinHandle<ToolOutcome>)> = segment
                .iter()
                .map(|&i| {
                    emit(
                        &self.events,
                        EngineEvent::ToolCallStart {
                            name: calls[i].name.clone(),
                            preview: preview(&calls[i].arguments, PREVIEW_LEN),
                        },
                    );
                    (i, self.spawn_call(&calls[i]))
                })
                .collect();

            for (i, handle) in handles {
                let outcome = match handle.await {
                    Ok(outcome) => outcome,
                    // A panic inside a tool is a local bug, not a loop killer.
                    Err(e) => ToolOutcome {
                        output: format!("tool dispatch panicked: {e}"),
                        is_error: true,
                    },
                };
                emit(
                    &self.events,
                    EngineEvent::ToolResult {
                        name: calls[i].name.clone(),
                        ok: !outcome.is_error,
                        preview: preview(&outcome.output, PREVIEW_LEN),
                    },
                );
                outcomes[i] = Some(outcome);
            }
        }

        outcomes
            .into_iter()
            .map(|o| {
                o.unwrap_or(ToolOutcome {
                    output: "internal: tool call was never dispatched".into(),
                    is_error: true,
                })
            })
            .collect()
    }

    fn spawn_call(&self, call: &ToolCall) -> JoinHandle<ToolOutcome> {
        let toolbox = self.toolbox.clone();
        let call = call.clone();
        tokio::spawn(async move { toolbox.dispatch(&call).await })
    }
}

/// Plan tool calls into segments preserving model order: consecutive
/// reader-only calls share a segment (run concurrently); any writer or
/// unclassified call (bash/grep/finish) gets a segment of its own.
fn plan_segments(calls: &[ToolCall]) -> Vec<Vec<usize>> {
    let mut segments: Vec<Vec<usize>> = Vec::new();
    let mut readers: Vec<usize> = Vec::new();

    for (i, call) in calls.iter().enumerate() {
        match ToolBox::path_access(call) {
            Some((_, false)) => readers.push(i),
            _ => {
                if !readers.is_empty() {
                    segments.push(std::mem::take(&mut readers));
                }
                segments.push(vec![i]);
            }
        }
    }
    if !readers.is_empty() {
        segments.push(readers);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::NormalizedResponse;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Scripted provider: pops one response per chat call, records requests.
    struct MockProvider {
        script: Mutex<VecDeque<NormalizedResponse>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl MockProvider {
        fn new(script: Vec<NormalizedResponse>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Provider for MockProvider {
        fn chat(
            &self,
            req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            self.requests.lock().unwrap().push(req);
            let resp = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("mock script exhausted");
            Box::pin(async move { Ok(resp) })
        }
    }

    fn tc(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    fn resp_tools(calls: Vec<ToolCall>) -> NormalizedResponse {
        NormalizedResponse {
            model: None,
            content: String::new(),
            tool_calls: calls,
            finish_reason: FinishReason::ToolCalls,
            reasoning: None,
            reasoning_details: None,
            usage: Default::default(),
        }
    }

    fn resp_text(text: &str, finish_reason: FinishReason) -> NormalizedResponse {
        NormalizedResponse {
            model: None,
            content: text.into(),
            tool_calls: vec![],
            finish_reason,
            reasoning: None,
            reasoning_details: None,
            usage: Default::default(),
        }
    }

    fn agent(provider: Arc<MockProvider>, root: &std::path::Path) -> ForgeAgent {
        ForgeAgent::new(provider, ToolBox::new(root), "test/model")
    }

    #[tokio::test]
    async fn read_edit_finish_drives_a_real_file_change() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("src.txt"), "hello world\n").unwrap();

        let provider = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "src.txt"}))]),
            resp_tools(vec![tc(
                "c2",
                "edit_file",
                serde_json::json!({"path": "src.txt", "old_string": "hello", "new_string": "goodbye"}),
            )]),
            resp_tools(vec![tc(
                "c3",
                "finish",
                serde_json::json!({"summary": "swapped greeting"}),
            )]),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "swap the greeting".into())
            .await
            .expect("run ok");

        assert_eq!(result, "swapped greeting");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src.txt")).unwrap(),
            "goodbye world\n"
        );
        // finish short-circuited: exactly 3 model calls, all with tools.
        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|r| !r.tools.is_empty()));
    }

    #[tokio::test]
    async fn every_tool_call_id_gets_a_result_even_on_error() {
        let dir = tempfile::tempdir().unwrap();

        let provider = MockProvider::new(vec![
            resp_tools(vec![
                tc("c1", "read_file", serde_json::json!({"path": "missing.txt"})),
                tc("c2", "bogus_tool", serde_json::json!({})),
            ]),
            resp_text("done", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "done");

        // The second request carries a tool result for BOTH ids.
        let requests = provider.requests();
        let msgs = &requests[1].messages;
        let tool_msgs: Vec<&ChatMessage> = msgs.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(tool_msgs.len(), 2);
        assert_eq!(tool_msgs[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(tool_msgs[1].tool_call_id.as_deref(), Some("c2"));
        assert!(tool_msgs[0].content.starts_with("ERROR: "));
        assert!(tool_msgs[1].content.starts_with("ERROR: "));
    }

    #[tokio::test]
    async fn max_turns_forces_a_final_no_tools_summary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let read = || tc("c1", "read_file", serde_json::json!({"path": "a.txt"}));
        let provider = MockProvider::new(vec![
            resp_tools(vec![read()]),
            resp_tools(vec![read()]),
            resp_text("summary after budget", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .with_max_turns(2)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "summary after budget");

        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        let last = &requests[2];
        assert!(last.tools.is_empty(), "final call must offer no tools");
        let last_user = last.messages.iter().rev().find(|m| m.role == "user").unwrap();
        assert!(last_user.content.contains("no more tool budget"));
    }

    #[tokio::test]
    async fn length_truncation_gets_exactly_one_continue() {
        let dir = tempfile::tempdir().unwrap();

        let provider = MockProvider::new(vec![
            resp_text("partial", FinishReason::Length),
            resp_text("complete", FinishReason::Stop),
        ]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "complete");

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let msgs = &requests[1].messages;
        assert_eq!(msgs[msgs.len() - 1].content, "continue");
        assert_eq!(msgs[msgs.len() - 2].content, "partial");
    }

    #[tokio::test]
    async fn second_length_truncation_returns_content() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![
            resp_text("partial", FinishReason::Length),
            resp_text("still partial", FinishReason::Length),
        ]);

        let result = agent(provider, dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "still partial");
    }

    #[tokio::test]
    async fn writer_closes_segment_so_batch_runs_in_model_order() {
        let dir = tempfile::tempdir().unwrap();

        // write then read of the same file in one batch: the writer must
        // complete before the read runs, so the read sees the new content.
        let provider = MockProvider::new(vec![
            resp_tools(vec![
                tc(
                    "c1",
                    "write_file",
                    serde_json::json!({"path": "f.txt", "content": "forged\n"}),
                ),
                tc("c2", "read_file", serde_json::json!({"path": "f.txt"})),
            ]),
            resp_text("done", FinishReason::Stop),
        ]);

        agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let requests = provider.requests();
        let read_result = requests[1]
            .messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c2"))
            .expect("read result present");
        assert!(
            read_result.content.contains("forged"),
            "read saw: {}",
            read_result.content
        );
    }

    #[test]
    fn plan_segments_groups_readers_and_isolates_writers() {
        let calls = vec![
            tc("1", "read_file", serde_json::json!({"path": "a"})),
            tc("2", "read_file", serde_json::json!({"path": "b"})),
            tc("3", "write_file", serde_json::json!({"path": "c", "content": ""})),
            tc("4", "bash", serde_json::json!({"command": "ls"})),
            tc("5", "read_file", serde_json::json!({"path": "d"})),
        ];
        let segments = plan_segments(&calls);
        assert_eq!(segments, vec![vec![0, 1], vec![2], vec![3], vec![4]]);
    }

    #[tokio::test]
    async fn finish_preserves_same_turn_assistant_text() {
        let dir = tempfile::tempdir().unwrap();

        // Plan mode contract: the plan arrives as final text on the finish
        // turn; only the one-line summary rides the finish call.
        let mut resp = resp_tools(vec![tc(
            "c1",
            "finish",
            serde_json::json!({"summary": "plan drafted"}),
        )]);
        resp.content = "## Plan\n1. do the thing".into();
        let provider = MockProvider::new(vec![resp]);

        let result = agent(provider, dir.path())
            .run("system".into(), "plan it".into())
            .await
            .expect("run ok");
        assert!(result.contains("## Plan\n1. do the thing"), "got: {result}");
        assert!(result.contains("plan drafted"), "got: {result}");
    }

    #[tokio::test]
    async fn reasoning_details_replay_with_assistant_tool_calls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let details = serde_json::json!([{"type": "reasoning.encrypted", "data": "opaque"}]);
        let mut first = resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]);
        first.reasoning_details = Some(details.clone());
        let provider = MockProvider::new(vec![first, resp_text("done", FinishReason::Stop)]);

        agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        // The second request must replay the reasoning blocks on the
        // assistant message that carried the tool calls.
        let requests = provider.requests();
        let assistant = requests[1]
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("assistant message present");
        assert_eq!(assistant.reasoning_details.as_ref(), Some(&details));
    }

    #[tokio::test]
    async fn finish_alongside_other_calls_still_answers_all_ids() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let provider = MockProvider::new(vec![resp_tools(vec![
            tc("c1", "read_file", serde_json::json!({"path": "a.txt"})),
            tc("c2", "finish", serde_json::json!({"summary": "all done"})),
        ])]);

        let result = agent(provider.clone(), dir.path())
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "all done");
        // Short-circuit: only one model call ever happened.
        assert_eq!(provider.requests().len(), 1);
    }

    /// Wraps MockProvider with a per-call hook — the only way to inject a
    /// steer or flip the cancel flag *between* turns of a running loop.
    struct HookedProvider {
        inner: Arc<MockProvider>,
        hook: Box<dyn Fn(usize) + Send + Sync>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Provider for HookedProvider {
        fn chat(
            &self,
            req: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>
        {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            (self.hook)(n);
            self.inner.chat(req)
        }
    }

    #[tokio::test]
    async fn steer_before_first_turn_becomes_a_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("done", FinishReason::Stop)]);
        let queue: SteerQueue = Arc::new(Mutex::new(vec!["focus on tests".into()]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let result = agent(provider.clone(), dir.path())
            .with_steer(queue.clone())
            .with_events(tx)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");
        assert_eq!(result, "done");
        assert!(queue.lock().unwrap().is_empty(), "queue drained");

        let msgs = &provider.requests()[0].messages;
        let steer_msg = msgs
            .iter()
            .find(|m| m.content.contains("focus on tests"))
            .expect("steer message present");
        assert_eq!(steer_msg.role, "user");
        assert!(steer_msg.content.starts_with(STEER_TAG));

        let mut saw_steer_event = false;
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::Steer { text } = ev {
                assert_eq!(text, "focus on tests");
                saw_steer_event = true;
            }
        }
        assert!(saw_steer_event, "Steer event emitted");
    }

    #[tokio::test]
    async fn multiple_first_turn_steers_keep_queue_order() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("done", FinishReason::Stop)]);
        let queue: SteerQueue = Arc::new(Mutex::new(vec!["first".into(), "second".into()]));

        agent(provider.clone(), dir.path())
            .with_steer(queue)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let msgs = &provider.requests()[0].messages;
        let first = msgs.iter().position(|m| m.content.ends_with("\nfirst")).unwrap();
        let second = msgs.iter().position(|m| m.content.ends_with("\nsecond")).unwrap();
        assert!(first < second, "steers applied in queue order");
    }

    #[tokio::test]
    async fn steers_during_tools_concatenate_into_last_tool_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let inner = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]),
            resp_text("done", FinishReason::Stop),
        ]);
        // Steers arrive during the first model call: after the turn-1 drain,
        // before the post-tool drain — they must land in the tool result.
        let queue: SteerQueue = Arc::new(Mutex::new(Vec::new()));
        let q = queue.clone();
        let provider = Arc::new(HookedProvider {
            inner: inner.clone(),
            hook: Box::new(move |n| {
                if n == 0 {
                    let mut steers = q.lock().unwrap();
                    steers.push("first".into());
                    steers.push("second".into());
                }
            }),
            calls: Default::default(),
        });

        ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_steer(queue)
            .run("system".into(), "task".into())
            .await
            .expect("run ok");

        let requests = inner.requests();
        let tool_msg = requests[1]
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "tool")
            .expect("tool result present");
        let i1 = tool_msg
            .content
            .find(&format!("{STEER_TAG}\nfirst"))
            .expect("first steer in tool result");
        let i2 = tool_msg
            .content
            .find(&format!("{STEER_TAG}\nsecond"))
            .expect("second steer in tool result");
        assert!(i1 > 0, "steer appended after the tool output");
        assert!(i1 < i2, "steers concatenate in order");
        // No steer leaked into a user message.
        assert!(requests[1]
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .all(|m| !m.content.contains(STEER_TAG)));
    }

    /// Scripted provider that can also fail: pops one Result per chat call.
    struct FlakyProvider {
        script: Mutex<VecDeque<Result<NormalizedResponse, String>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl FlakyProvider {
        fn new(script: Vec<Result<NormalizedResponse, String>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    impl Provider for FlakyProvider {
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
                .expect("flaky script exhausted");
            Box::pin(async move { next.map_err(SlagError::Provider) })
        }
    }

    #[test]
    fn context_overflow_classifier_matches_provider_phrasings() {
        for msg in [
            "400 Bad Request: This endpoint's maximum context length is 131072 tokens",
            "400: context_length_exceeded",
            "400: input is too long for this model's context window",
        ] {
            assert!(is_context_overflow(&SlagError::Provider(msg.into())), "{msg}");
        }
        assert!(!is_context_overflow(&SlagError::Provider("429: rate limited".into())));
        assert!(!is_context_overflow(&SlagError::Provider("500: internal".into())));
    }

    #[tokio::test]
    async fn context_overflow_compacts_harder_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), format!("{}\n", "y".repeat(6000))).unwrap();

        let read = || tc("c1", "read_file", serde_json::json!({"path": "big.txt"}));
        // Six fat read turns build prunable history beyond the protected
        // tail, then the provider rejects on context, then accepts the
        // harder-compacted retry.
        let provider = FlakyProvider::new(vec![
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Ok(resp_tools(vec![read()])),
            Err("400 Bad Request: maximum context length is 8192 tokens".into()),
            Ok(resp_text("done", FinishReason::Stop)),
        ]);

        let result = ForgeAgent::new(provider.clone(), ToolBox::new(dir.path()), "test/model")
            .with_char_budget(40_000)
            .run("system".into(), "task".into())
            .await
            .expect("overflow must be recovered by compaction, not propagated");
        assert_eq!(result, "done");

        let requests = provider.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 8, "exactly one retry after the overflow");
        // The retry request carries a pruned (stubbed) old tool result.
        let retry = &requests[7];
        assert!(
            retry.messages.iter().any(|m| m.content.starts_with("[pruned old tool result")),
            "retry must be compacted"
        );
    }

    #[tokio::test]
    async fn first_turn_overflow_with_nothing_prunable_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FlakyProvider::new(vec![
            Err("400: maximum context length exceeded".into()),
        ]);
        let err = ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_char_budget(40_000)
            .run("system".into(), "task".into())
            .await
            .expect_err("nothing prunable — must fail, not loop");
        assert!(err.to_string().contains("context"), "got: {err}");
    }

    #[tokio::test]
    async fn steers_are_requeued_when_the_provider_errors() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FlakyProvider::new(vec![Err("500: boom".into())]);
        let queue: SteerQueue = Arc::new(Mutex::new(vec!["do NOT touch migrations".into()]));

        let err = ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_steer(queue.clone())
            .run("system".into(), "task".into())
            .await
            .expect_err("provider error propagates");
        assert!(err.to_string().contains("boom"), "got: {err}");

        // The drained steer is back in the queue for the retry heat.
        assert_eq!(*queue.lock().unwrap(), vec!["do NOT touch migrations".to_string()]);
    }

    #[tokio::test]
    async fn preset_cancel_aborts_before_any_provider_call() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockProvider::new(vec![resp_text("never", FinishReason::Stop)]);
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let err = agent(provider.clone(), dir.path())
            .with_cancel(flag)
            .run("system".into(), "task".into())
            .await
            .expect_err("cancel must abort the run");
        assert!(err.to_string().contains("interrupted by user"), "got: {err}");
        assert_eq!(provider.requests().len(), 0, "no provider call after cancel");
    }

    #[tokio::test]
    async fn mid_run_cancel_aborts_before_the_next_provider_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();

        let inner = MockProvider::new(vec![
            resp_tools(vec![tc("c1", "read_file", serde_json::json!({"path": "a.txt"}))]),
            resp_text("never reached", FinishReason::Stop),
        ]);
        // Flag flips during the first model call: tools still run, but the
        // turn-2 boundary check must stop the loop.
        let flag: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = flag.clone();
        let provider = Arc::new(HookedProvider {
            inner: inner.clone(),
            hook: Box::new(move |n| {
                if n == 0 {
                    f.store(true, Ordering::SeqCst);
                }
            }),
            calls: Default::default(),
        });

        let err = ForgeAgent::new(provider, ToolBox::new(dir.path()), "test/model")
            .with_cancel(flag)
            .run("system".into(), "task".into())
            .await
            .expect_err("cancel must abort the run");
        assert!(err.to_string().contains("interrupted by user"), "got: {err}");
        assert_eq!(inner.requests().len(), 1, "exactly one call before the flag check");
    }
}
