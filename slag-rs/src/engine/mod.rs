//! engine — slag's native forging engine.
//!
//! OpenRouter-backed agentic loop. This module replaces external CLI smiths.
//! Shared contracts live here; submodules implement them.
#![allow(dead_code)]

pub mod agent;
pub mod compact;
pub mod events;
pub mod prompt;
pub mod provider;
pub mod tools;

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::error::SlagError;

pub const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";

/// Reasoning effort forwarded to OpenRouter's unified `reasoning` param.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn from_grade(grade: u8) -> Self {
        match grade {
            0..=2 => Effort::Low,
            3 => Effort::Medium,
            _ => Effort::High,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

/// A tool invocation as it appears on the OpenAI-compatible wire.
/// `arguments` stays a raw JSON string; `tools::ToolBox` parses it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// One chat message. Wire-compatible with OpenAI chat/completions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Base64 data URLs. Never serialized directly: the provider expands
    /// content into multimodal parts when images are present.
    #[serde(skip)]
    pub images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Raw OpenRouter reasoning blocks. Anthropic/Gemini reasoning models
    /// require these replayed verbatim with the assistant tool_calls
    /// message, or the follow-up request is rejected.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reasoning_details: Option<serde_json::Value>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into(), tool_calls: None, tool_call_id: None, reasoning_details: None, images: None }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into(), tool_calls: None, tool_call_id: None, reasoning_details: None, images: None }
    }
    pub fn assistant(content: impl Into<String>, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self { role: "assistant".into(), content: content.into(), tool_calls, tool_call_id: None, reasoning_details: None, images: None }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            reasoning_details: None,
            images: None,
        }
    }
    pub fn with_reasoning_details(mut self, details: Option<serde_json::Value>) -> Self {
        self.reasoning_details = details;
        self
    }
}

/// Tool schema advertised to the model.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// OpenRouter reports spend in credits when `usage: {include: true}`.
    #[serde(default)]
    pub cost: Option<f64>,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        if let Some(c) = other.cost {
            *self.cost.get_or_insert(0.0) += c;
        }
    }
}

/// Provider-agnostic model response (hermes `NormalizedResponse` pattern).
#[derive(Debug, Clone)]
pub struct NormalizedResponse {
    /// Model the provider actually ran, when it says. Differs from the
    /// requested id whenever a router like `openrouter/auto` is in play.
    pub model: Option<String>,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub reasoning: Option<String>,
    /// Raw reasoning blocks, preserved for replay (see `ChatMessage`).
    pub reasoning_details: Option<serde_json::Value>,
    pub usage: Usage,
}

/// One model call.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub effort: Option<Effort>,
    pub max_tokens: Option<u32>,
}

/// Model provider boundary. Boxed future for dyn compatibility,
/// same style as `crate::smith::Smith`.
pub trait Provider: Send + Sync {
    fn chat(
        &self,
        req: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>>;
}

/// Typed engine events. One stream feeds TUI, JSONL logs, and --json mode.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    TurnStart { turn: usize },
    ModelCall { model: String },
    /// A router answered with a different model than the one requested.
    /// Only `openrouter/auto` and friends emit this; a pinned model never
    /// does, so this line always carries news.
    ModelRouted { requested: String, routed: String },
    ToolCallStart { name: String, preview: String },
    ToolResult { name: String, ok: bool, preview: String },
    Tokens { usage: Usage },
    Steer { text: String },
    Finish { summary: String },
    Error { message: String },
    // Pipeline-level events (emitted by forge, consumed by the dashboard).
    IngotStart { id: String, work: String },
    HeatTick { id: String, heat: u8 },
    IngotDone { id: String, ok: bool },
    DuelRound { id: String, round: u8 },
    DuelVerdict { id: String, winner: char, margin: u8 },
}

/// Steering messages queued by the TUI, drained by the agent loop before
/// each model call (hermes steer-into-tool-result pattern).
pub type SteerQueue = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// Hard-interrupt flag checked at each turn boundary.
pub type CancelFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;

/// Assayer verdict for one duel round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// 'a' or 'b'
    pub winner: char,
    pub score_a: u8,
    pub score_b: u8,
    /// What the loser did better — injected into the next round.
    pub critique: String,
}

impl Verdict {
    pub fn margin(&self) -> u8 {
        self.score_a.abs_diff(self.score_b)
    }
}

pub type EventTx = tokio::sync::mpsc::UnboundedSender<EngineEvent>;

/// Emit an event, ignoring a closed channel (engine must never die on display).
pub fn emit(tx: &Option<EventTx>, event: EngineEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Outcome of one tool dispatch.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
}
