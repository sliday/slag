//! provider — OpenRouter HTTP client for the native forging engine.
//!
//! OpenAI-compatible wire format. Retries 429/5xx with exponential backoff,
//! fails fast on other 4xx. Provider quirks stay quarantined here; the rest
//! of the engine sees only `NormalizedResponse`.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use super::{ChatMessage, ChatRequest, FinishReason, NormalizedResponse, Provider, ToolCall, Usage};
use crate::error::SlagError;

const REQUEST_TIMEOUT_SECS: u64 = 600;
const MAX_ATTEMPTS: usize = 3;
const BACKOFF_MS: [u64; 2] = [250, 1000];
const BODY_EXCERPT_LEN: usize = 300;

/// OpenRouter chat-completions client.
pub struct OpenRouter {
    api_key: String,
    base_url: String,
    http: reqwest::Client,
}

impl OpenRouter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, super::OPENROUTER_BASE)
    }

    /// Base URL override enables wiremock tests and proxies.
    pub fn with_base_url(api_key: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .build()
                .expect("reqwest client build"),
        }
    }

    fn build_body(req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "messages": wire_messages(&req.messages),
            "usage": { "include": true },
        });
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        },
                    })
                })
                .collect();
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(effort) = req.effort {
            body["reasoning"] = json!({ "effort": effort.as_str() });
        }
        if let Some(max) = req.max_tokens {
            body["max_tokens"] = json!(max);
        }
        body
    }

    async fn chat_impl(&self, req: ChatRequest) -> Result<NormalizedResponse, SlagError> {
        let body = Self::build_body(&req);
        let url = format!("{}/chat/completions", self.base_url);
        let mut last_err = String::new();

        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(BACKOFF_MS[attempt - 1])).await;
            }

            let sent = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .header("HTTP-Referer", "https://slag.dev")
                .header("X-Title", "slag")
                .json(&body)
                .send()
                .await;

            let resp = match sent {
                Ok(resp) => resp,
                Err(e) => {
                    last_err = format!("request failed: {e}");
                    continue;
                }
            };

            let status = resp.status();
            if status.is_success() {
                let api: ApiResponse = resp
                    .json()
                    .await
                    .map_err(|e| SlagError::Provider(format!("malformed response: {e}")))?;
                return normalize(api);
            }

            let text = resp.text().await.unwrap_or_default();
            let excerpt = excerpt(&text);
            if status.as_u16() == 429 || status.is_server_error() {
                last_err = format!("{status}: {excerpt}");
                continue;
            }
            return Err(SlagError::Provider(format!("{status}: {excerpt}")));
        }

        Err(SlagError::Provider(format!(
            "gave up after {MAX_ATTEMPTS} attempts: {last_err}"
        )))
    }
}

impl Provider for OpenRouter {
    fn chat(
        &self,
        req: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>> {
        Box::pin(async move { self.chat_impl(req).await })
    }
}

/// Cheap key check: GET {base}/models with the bearer key, 200 = valid.
pub async fn validate_key(api_key: &str, base_url: &str) -> Result<(), SlagError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| SlagError::Provider(format!("client build failed: {e}")))?;
    let resp = client
        .get(&url)
        .bearer_auth(api_key)
        .header("HTTP-Referer", "https://slag.dev")
        .header("X-Title", "slag")
        .send()
        .await
        .map_err(|e| SlagError::Provider(format!("key validation request failed: {e}")))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(SlagError::Provider(format!(
            "key validation failed: {}",
            resp.status()
        )))
    }
}

/// Map `ChatMessage` onto the OpenAI wire shape (assistant tool_calls nest
/// under `function`; the frozen `ChatMessage` serialization keeps them flat).
/// Messages carrying images expand `content` into multimodal parts.
fn wire_messages(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let content = match m.images.as_deref().filter(|imgs| !imgs.is_empty()) {
                Some(images) => {
                    let mut parts = vec![json!({ "type": "text", "text": m.content })];
                    for url in images {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url },
                        }));
                    }
                    Value::Array(parts)
                }
                None => json!(m.content),
            };
            let mut obj = json!({ "role": m.role, "content": content });
            if let Some(calls) = &m.tool_calls {
                obj["tool_calls"] = Value::Array(
                    calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.arguments },
                            })
                        })
                        .collect(),
                );
            }
            if let Some(id) = &m.tool_call_id {
                obj["tool_call_id"] = json!(id);
            }
            if let Some(details) = &m.reasoning_details {
                obj["reasoning_details"] = details.clone();
            }
            obj
        })
        .collect()
}

fn normalize(api: ApiResponse) -> Result<NormalizedResponse, SlagError> {
    let choice = api
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| SlagError::Provider("no choices in response".into()))?;
    let msg = choice.message;

    let tool_calls: Vec<ToolCall> = msg
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, tc)| ToolCall {
            // Some upstreams omit ids; strict backends reject "" on replay.
            id: if tc.id.is_empty() { format!("call_{i}") } else { tc.id },
            name: tc.function.name,
            arguments: tc.function.arguments,
        })
        .collect();

    let finish_reason = match choice.finish_reason.as_deref() {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        _ => FinishReason::Other,
    };

    let reasoning = msg
        .reasoning
        .filter(|s| !s.is_empty())
        .or_else(|| reasoning_from_details(msg.reasoning_details.as_ref()));

    Ok(NormalizedResponse {
        content: msg.content.unwrap_or_default(),
        tool_calls,
        finish_reason,
        reasoning,
        reasoning_details: msg.reasoning_details,
        usage: api.usage.unwrap_or_default(),
    })
}

/// Best-effort extraction from OpenRouter `reasoning_details` blocks.
fn reasoning_from_details(details: Option<&Value>) -> Option<String> {
    let arr = details?.as_array()?;
    let texts: Vec<&str> = arr
        .iter()
        .filter_map(|d| {
            d.get("text")
                .or_else(|| d.get("summary"))
                .and_then(Value::as_str)
        })
        .filter(|s| !s.is_empty())
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= BODY_EXCERPT_LEN {
        trimmed.to_string()
    } else {
        let mut end = BODY_EXCERPT_LEN;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    choices: Vec<ApiChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ApiChoice {
    message: ApiMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ApiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ApiToolCall>>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_details: Option<Value>,
}

#[derive(Deserialize)]
struct ApiToolCall {
    #[serde(default)]
    id: String,
    function: ApiFunction,
}

#[derive(Deserialize)]
struct ApiFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Effort, ToolSpec};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn spec() -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        }
    }

    fn request(tools: Vec<ToolSpec>, effort: Option<Effort>) -> ChatRequest {
        ChatRequest {
            model: "qwen/qwen3-coder".into(),
            messages: vec![
                ChatMessage::system("you are slag"),
                ChatMessage::user("forge it"),
            ],
            tools,
            effort,
            max_tokens: None,
        }
    }

    #[test]
    fn body_wraps_tools_and_sets_tool_choice() {
        let body = OpenRouter::build_body(&request(vec![spec()], Some(Effort::High)));
        assert_eq!(body["tool_choice"], json!("auto"));
        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("read_file"));
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        assert_eq!(body["usage"], json!({ "include": true }));
    }

    #[test]
    fn body_omits_tool_choice_and_reasoning_when_absent() {
        let body = OpenRouter::build_body(&request(vec![], None));
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn body_nests_assistant_tool_calls_under_function() {
        let mut req = request(vec![], None);
        req.messages.push(ChatMessage::assistant(
            "",
            Some(vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"a\"}".into(),
            }]),
        ));
        req.messages.push(ChatMessage::tool_result("call_1", "ok"));
        let body = OpenRouter::build_body(&req);
        let assistant = &body["messages"][2];
        assert_eq!(assistant["tool_calls"][0]["type"], json!("function"));
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            json!("read_file")
        );
        assert_eq!(body["messages"][3]["tool_call_id"], json!("call_1"));
    }

    #[test]
    fn body_replays_reasoning_details_on_assistant_message() {
        let details = json!([{"type": "reasoning.encrypted", "data": "opaque"}]);
        let mut req = request(vec![], None);
        req.messages.push(
            ChatMessage::assistant(
                "",
                Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"a\"}".into(),
                }]),
            )
            .with_reasoning_details(Some(details.clone())),
        );
        let body = OpenRouter::build_body(&req);
        assert_eq!(body["messages"][2]["reasoning_details"], details);
        // Messages without details must not carry the key at all.
        assert!(body["messages"][0].get("reasoning_details").is_none());
    }

    #[test]
    fn body_expands_images_into_multimodal_parts() {
        let mut req = request(vec![], None);
        let mut user = ChatMessage::user("compare the casts");
        user.images = Some(vec!["data:image/png;base64,QUJD".into()]);
        req.messages.push(user);
        let body = OpenRouter::build_body(&req);
        // Plain messages keep string content.
        assert_eq!(body["messages"][1]["content"], json!("forge it"));
        // Image-bearing message becomes [text part, image_url part].
        let parts = &body["messages"][2]["content"];
        assert_eq!(parts[0], json!({ "type": "text", "text": "compare the casts" }));
        assert_eq!(
            parts[1],
            json!({
                "type": "image_url",
                "image_url": { "url": "data:image/png;base64,QUJD" },
            })
        );
    }

    #[tokio::test]
    async fn multimodal_message_sends_parts_on_the_wire() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [
                    { "role": "system", "content": "you are slag" },
                    { "role": "user", "content": "forge it" },
                    { "role": "user", "content": [
                        { "type": "text", "text": "compare the casts" },
                        { "type": "image_url", "image_url": { "url": "data:image/png;base64,QUJD" } },
                        { "type": "image_url", "image_url": { "url": "data:image/png;base64,REVG" } },
                    ]},
                ],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "content": "cast a wins" },
                    "finish_reason": "stop",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut req = request(vec![], None);
        let mut user = ChatMessage::user("compare the casts");
        user.images = Some(vec![
            "data:image/png;base64,QUJD".into(),
            "data:image/png;base64,REVG".into(),
        ]);
        req.messages.push(user);

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider.chat(req).await.expect("chat ok");
        assert_eq!(resp.content, "cast a wins");
    }

    #[tokio::test]
    async fn happy_path_parses_tool_calls_and_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({
                "tool_choice": "auto",
                "usage": { "include": true },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"src/main.rs\"}",
                            },
                        }],
                    },
                    "finish_reason": "tool_calls",
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "cost": 0.0012,
                },
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider
            .chat(request(vec![spec()], None))
            .await
            .expect("chat ok");

        assert_eq!(resp.content, "");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "call_1");
        assert_eq!(resp.tool_calls[0].name, "read_file");
        assert_eq!(resp.tool_calls[0].arguments, "{\"path\":\"src/main.rs\"}");
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.usage.total_tokens, 15);
        assert_eq!(resp.usage.cost, Some(0.0012));
    }

    #[tokio::test]
    async fn retries_429_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "content": "forged" },
                    "finish_reason": "stop",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider
            .chat(request(vec![], None))
            .await
            .expect("retry then success");

        assert_eq!(resp.content, "forged");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn fails_fast_on_401_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-bad", server.uri());
        let err = provider
            .chat(request(vec![], None))
            .await
            .expect_err("401 must fail");

        let msg = err.to_string();
        assert!(msg.contains("401"), "message was: {msg}");
        assert!(msg.contains("invalid api key"), "message was: {msg}");
    }

    #[tokio::test]
    async fn parses_reasoning_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "content": "done",
                        "reasoning": "check the proof first",
                    },
                    "finish_reason": "stop",
                }],
            })))
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider
            .chat(request(vec![], Some(Effort::Medium)))
            .await
            .expect("chat ok");

        assert_eq!(resp.reasoning.as_deref(), Some("check the proof first"));
    }

    #[tokio::test]
    async fn parses_reasoning_details_when_reasoning_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "content": "done",
                        "reasoning_details": [
                            { "type": "reasoning.text", "text": "step one" },
                            { "type": "reasoning.text", "text": "step two" },
                        ],
                    },
                    "finish_reason": "stop",
                }],
            })))
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider
            .chat(request(vec![], None))
            .await
            .expect("chat ok");

        assert_eq!(resp.reasoning.as_deref(), Some("step one\nstep two"));
        // Raw blocks preserved for replay on the next turn.
        let details = resp.reasoning_details.expect("raw details kept");
        assert_eq!(details[0]["text"], json!("step one"));
    }

    #[tokio::test]
    async fn tolerates_null_content_and_missing_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "content": null },
                    "finish_reason": "stop",
                }],
            })))
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider
            .chat(request(vec![], None))
            .await
            .expect("chat ok");

        assert_eq!(resp.content, "");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.total_tokens, 0);
        assert!(resp.usage.cost.is_none());
    }

    #[tokio::test]
    async fn validate_key_accepts_200_rejects_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .mount(&server)
            .await;

        validate_key("sk-test", &server.uri())
            .await
            .expect("valid key");

        let bad = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&bad)
            .await;

        let err = validate_key("sk-bad", &bad.uri())
            .await
            .expect_err("bad key");
        assert!(err.to_string().contains("401"));
    }
}
