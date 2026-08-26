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

use super::{
    CancelFlag, ChatMessage, ChatRequest, EngineEvent, EventTx, FinishReason, NormalizedResponse,
    Provider, RetryPolicy, ToolCall, Usage,
};
use crate::error::{ProviderApiError, ProviderErrorCategory, SlagError};

const DEFAULT_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_MAX_ATTEMPTS: usize = 8;
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CAP_MS: u64 = 32_000;
const RETRY_AFTER_CAP_SECS: u64 = 60;
const BODY_EXCERPT_LEN: usize = 300;
/// Unattended mode: capacity backoff caps at 5 minutes per wait…
const UNATTENDED_BACKOFF_CAP_MS: u64 = 300_000;
/// …a server reset timestamp is honored up to an hour…
const UNATTENDED_WAIT_CEILING: Duration = Duration::from_secs(60 * 60);
/// …and cumulative capacity waiting stops at 6 hours, so a dead account
/// cannot hang an unattended forge forever.
const UNATTENDED_TOTAL_CEILING: Duration = Duration::from_secs(6 * 60 * 60);
/// Long unattended waits sleep in 30s slices, one `ApiRetry` heartbeat
/// per slice, so the dashboard and JSONL logs stay alive.
const HEARTBEAT_SLICE: Duration = Duration::from_secs(30);
/// Floor for unattended capacity waits. A server-sent `Retry-After: 0`
/// (OpenRouter's daily free-tier limit does this) or a reset timestamp a
/// few ms ahead must not become a zero-delay request storm: every free
/// retry waits at least this long, so `unattended_waited` strictly grows
/// and the cumulative 6h ceiling always terminates the loop.
const UNATTENDED_MIN_DELAY: Duration = Duration::from_secs(1);

/// Retry budget: `SLAG_MAX_RETRIES` overrides the default. Three attempts
/// over ~1.5s proved far too brittle for overnight forge runs now that
/// backoff is exponential — eight attempts ride the 500ms→32s curve.
fn parse_max_attempts(raw: Option<String>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_MAX_ATTEMPTS)
}

fn max_attempts_from_env() -> usize {
    parse_max_attempts(std::env::var("SLAG_MAX_RETRIES").ok())
}

/// Request timeout: `SLAG_API_TIMEOUT_MS` overrides the ~300s default.
/// Zero and garbage fall back rather than disabling the timeout.
fn parse_timeout_ms(raw: Option<String>) -> Duration {
    Duration::from_millis(
        raw.and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS),
    )
}

fn timeout_from_env() -> Duration {
    parse_timeout_ms(std::env::var("SLAG_API_TIMEOUT_MS").ok())
}

/// Build the HTTP client. `drop_idle_pool` disables connection reuse for
/// the client that replaces one whose pooled connections went stale.
fn build_client(timeout: Duration, drop_idle_pool: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    if drop_idle_pool {
        builder = builder.pool_max_idle_per_host(0);
    }
    builder.build().expect("reqwest client build")
}

/// OpenRouter chat-completions client.
pub struct OpenRouter {
    api_key: String,
    base_url: String,
    /// Behind a mutex so a stale-connection retry can swap in a fresh
    /// client (`rebuild_client`) without `&mut self`.
    http: std::sync::Mutex<reqwest::Client>,
    timeout: Duration,
    max_attempts: usize,
    /// Second entry of OpenRouter's native `models: [primary, fallback]`
    /// routing array — capacity failover inside one request.
    fallback_model: Option<String>,
    /// Unattended persistent-retry mode: 429/529 retry past the attempt
    /// budget (see `plan_retry`).
    unattended: bool,
    /// Event sink for `ApiRetry` heartbeats, when wired.
    events: std::sync::Mutex<Option<EventTx>>,
    /// Cancel flag, when wired: checked between heartbeat slices and after
    /// bounded waits, so a Ctrl-C ends a retry wait instead of sleeping it
    /// out and firing more (billable) requests.
    cancel: std::sync::Mutex<Option<CancelFlag>>,
    /// The `/models` index: context windows (compaction budget) and prices
    /// (item 34), resolved together from one fetch and cached so a client
    /// never pulls the large model list twice.
    models: tokio::sync::Mutex<Option<std::sync::Arc<ModelsIndex>>>,
}

impl OpenRouter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, super::OPENROUTER_BASE)
    }

    /// Base URL override enables wiremock tests and proxies.
    pub fn with_base_url(api_key: impl Into<String>, url: impl Into<String>) -> Self {
        let timeout = timeout_from_env();
        Self {
            api_key: api_key.into(),
            base_url: url.into().trim_end_matches('/').to_string(),
            http: std::sync::Mutex::new(build_client(timeout, false)),
            timeout,
            max_attempts: max_attempts_from_env(),
            fallback_model: crate::config::fallback_model(),
            unattended: crate::config::unattended_retry(),
            events: std::sync::Mutex::new(None),
            cancel: std::sync::Mutex::new(None),
            models: tokio::sync::Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_max_attempts(mut self, attempts: usize) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self.http = std::sync::Mutex::new(build_client(timeout, false));
        self
    }

    /// Pin the fallback (tests must not depend on ambient env).
    #[cfg(test)]
    fn with_fallback(mut self, fallback: Option<&str>) -> Self {
        self.fallback_model = fallback.map(str::to_string);
        self
    }

    #[cfg(test)]
    fn with_unattended(mut self, unattended: bool) -> Self {
        self.unattended = unattended;
        self
    }

    /// Swap in a fresh client with no idle pool: a connect/reset failure
    /// usually means the pooled connections went stale (server rotated
    /// or dropped them), and retrying on the same pool replays the
    /// failure instead of healing it.
    fn rebuild_client(&self) {
        *self.http.lock().unwrap() = build_client(self.timeout, true);
    }

    fn emit_heartbeat(&self, attempt: usize, status: u16, remaining_secs: u64) {
        if let Some(tx) = self.events.lock().unwrap().as_ref() {
            let _ = tx.send(EngineEvent::ApiRetry { attempt, status, remaining_secs });
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst))
    }

    #[cfg(test)]
    pub(crate) fn has_event_sink(&self) -> bool {
        self.events.lock().unwrap().is_some()
    }

    #[cfg(test)]
    pub(crate) fn has_cancel_flag(&self) -> bool {
        self.cancel.lock().unwrap().is_some()
    }

    /// Context window (tokens) for `model`, cached per client. `None` when
    /// the model list is unreachable or the id is unknown — window size is
    /// an optimization for the compaction budget, never a blocker.
    pub async fn context_length(&self, model: &str) -> Option<u64> {
        self.models_index().await.window(model)
    }

    /// The `/models` index, fetched at most once per client. Both the
    /// window cache and the price table come out of this single request.
    /// A failed fetch still caches (empty windows, disk-cached prices), so
    /// an offline run does not re-fetch on every turn.
    async fn models_index(&self) -> std::sync::Arc<ModelsIndex> {
        let mut slot = self.models.lock().await;
        if let Some(index) = slot.as_ref() {
            return index.clone();
        }
        let body = fetch_models_body(&self.base_url).await;
        let mut index = ModelsIndex::parse(body.as_deref());
        if index.prices.is_empty() {
            // No live prices: an offline run can still estimate from the
            // last table we saw.
            if let Some(cached) = crate::engine::pricing::load_cached() {
                index.prices = cached;
            }
        } else {
            crate::engine::pricing::store(&index.prices);
        }
        let index = std::sync::Arc::new(index);
        *slot = Some(index.clone());
        index
    }

    /// Stamp ledger provenance on the response's usage, fill a missing
    /// cost from the local price table (item 34), and fold the result into
    /// the run ledger (item 35). The model key prefers what the provider
    /// says it ran: with a router like `openrouter/auto` the requested id
    /// is not a priced model at all.
    ///
    /// This is the only success path out of `chat`, which is why the fold
    /// belongs here. The judge and the summarizer hold a provider directly
    /// rather than a `ForgeAgent`, so a fold in the agent loop misses them
    /// and their rows vanish from the assay.
    async fn attribute(&self, resp: &mut NormalizedResponse, req: &ChatRequest) {
        let ran = resp.model.clone().unwrap_or_else(|| req.model.clone());
        resp.usage.role = Some(req.role);
        resp.usage.model = Some(ran.clone());
        if resp.usage.cost.is_none() {
            if let Some(cost) = self.models_index().await.prices.estimate(&ran, &resp.usage) {
                resp.usage.cost = Some(cost);
                resp.usage.estimated = true;
            }
        }
        crate::engine::stats::record_usage(&resp.usage);
    }

    fn build_body(&self, req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "messages": wire_messages(&req.messages),
            "usage": { "include": true },
        });
        // OpenRouter's native fallback routing: when the primary sheds
        // load (429/529-class), the router retries the same request on
        // the fallback inside one round trip — no client-side retry turn
        // needed. The response's `model` field then names the fallback,
        // which the agent already surfaces as a `ModelRouted` event.
        if let Some(fb) = self.fallback_model.as_deref().filter(|fb| *fb != req.model) {
            body["models"] = json!([req.model, fb]);
        }
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
        // Sent unconditionally on purpose. OpenRouter drops parameters the
        // chosen model does not support (that is the default; only
        // `provider.require_parameters` makes them routing constraints), so
        // asking a non-reasoning model for effort costs nothing. Setting
        // require_parameters here would instead shrink the auto router's
        // pool to reasoning models only.
        if let Some(effort) = req.effort {
            body["reasoning"] = json!({ "effort": effort.as_str() });
        }
        if let Some(max) = req.max_tokens {
            body["max_tokens"] = json!(max);
        }
        body
    }

    /// Wait out one retry delay. Unattended (heartbeat) waits sleep in
    /// 30s slices, one `ApiRetry` event per slice, so the dashboard and
    /// JSONL logs stay alive through a minutes-long rate-limit window.
    /// Returns false when the cancel flag was raised — the caller must
    /// stop instead of firing another request.
    async fn wait_out(&self, attempt: usize, plan: &RetryPlan) -> bool {
        if !plan.heartbeats {
            tokio::time::sleep(plan.delay).await;
            return !self.is_cancelled();
        }
        for (remaining_secs, slice) in heartbeat_slices(plan.delay) {
            if self.is_cancelled() {
                return false;
            }
            self.emit_heartbeat(attempt, plan.status, remaining_secs);
            tokio::time::sleep(slice).await;
        }
        !self.is_cancelled()
    }

    /// Decide whether one more retry happens and how long to wait.
    ///
    /// Bounded failures ride the 500ms→32s curve until the attempt budget
    /// runs out. In unattended mode, capacity errors (429/529) are free —
    /// they never consume the budget — and wait until the server's
    /// rate-limit reset timestamp when it sent one (no polling), else
    /// Retry-After, else backoff capped at 5 minutes; a cumulative 6h
    /// ceiling still ends a hopeless wait. `None` = stop retrying.
    ///
    /// `policy` is the per-request override (item 50): a side call caps its
    /// own attempts and opts out of the free unattended wait, so a judge or
    /// summary request cannot multiply load across every anvil.
    fn plan_retry(
        &self,
        status: Option<u16>,
        retry_after: Option<Duration>,
        reset_wait: Option<Duration>,
        attempts_made: usize,
        budget_used: usize,
        unattended_waited: Duration,
        policy: RetryPolicy,
    ) -> Option<RetryPlan> {
        let capacity = matches!(status, Some(429) | Some(529));
        if self.unattended && capacity && policy.persistent {
            // A zero server hint (`Retry-After: 0`) means "the limit is
            // still on" here, not "retry now": treat it as absent so the
            // backoff curve applies, and floor whatever remains so every
            // free retry advances `unattended_waited` toward the ceiling.
            let delay = reset_wait
                .or(retry_after)
                .filter(|d| !d.is_zero())
                .unwrap_or_else(|| {
                    backoff_delay_capped(attempts_made, UNATTENDED_BACKOFF_CAP_MS)
                })
                .clamp(UNATTENDED_MIN_DELAY, UNATTENDED_WAIT_CEILING);
            if unattended_waited + delay > UNATTENDED_TOTAL_CEILING {
                return None;
            }
            return Some(RetryPlan {
                delay,
                status: status.unwrap_or(0),
                free: true,
                heartbeats: true,
            });
        }
        if budget_used >= policy.attempts.unwrap_or(self.max_attempts) {
            return None;
        }
        Some(RetryPlan {
            // A server-sent Retry-After beats the computed backoff. The
            // reset timestamp is ignored here on purpose: a bounded retry
            // must not silently sleep for minutes.
            delay: retry_after.unwrap_or_else(|| backoff_delay(attempts_made)),
            status: status.unwrap_or(0),
            free: false,
            heartbeats: false,
        })
    }

    async fn chat_impl(&self, req: ChatRequest) -> Result<NormalizedResponse, SlagError> {
        let body = self.build_body(&req);
        let url = format!("{}/chat/completions", self.base_url);
        // Definite-assignment checked: every `break` below writes it first.
        let mut last_err;
        let mut attempts_made = 0usize;
        let mut budget_used = 0usize;
        let mut unattended_waited = Duration::ZERO;
        // Set by the previous failure's plan: how to wait before this
        // attempt and whether it consumes the bounded budget.
        let mut next: Option<RetryPlan> = None;

        loop {
            let free = match next.take() {
                Some(plan) => {
                    if !self.wait_out(attempts_made, &plan).await {
                        return Err(SlagError::Cancelled);
                    }
                    if plan.heartbeats {
                        unattended_waited += plan.delay;
                    }
                    plan.free
                }
                None => false,
            };
            attempts_made += 1;
            if !free {
                budget_used += 1;
            }

            let client = self.http.lock().unwrap().clone();
            let sent = client
                .post(&url)
                .bearer_auth(&self.api_key)
                .header("HTTP-Referer", "https://slag.dev")
                .header("X-Title", "slag")
                .json(&body)
                .send()
                .await;

            let resp = match sent {
                Ok(resp) => resp,
                // Connect and timeout failures are transient by definition;
                // the remaining send errors rarely heal but a bounded retry
                // costs little.
                Err(e) => {
                    if is_stale_connection(&e) {
                        self.rebuild_client();
                    }
                    last_err = format!("request failed: {e}");
                    match self.plan_retry(None, None, None, attempts_made, budget_used, unattended_waited, req.retry) {
                        Some(plan) => {
                            next = Some(plan);
                            continue;
                        }
                        None => break,
                    }
                }
            };

            let status = resp.status();
            let retry_hint = should_retry_override(resp.headers());
            if status.is_success() {
                let api: ApiResponse = match resp.json().await {
                    Ok(api) => api,
                    Err(e) => {
                        return Err(SlagError::ProviderApi(ProviderApiError {
                            status: Some(200),
                            category: ProviderErrorCategory::BadResponse,
                            retryable: false,
                            excerpt: format!("malformed response: {e}"),
                        }))
                    }
                };
                match normalize(api) {
                    // Empty 200s (no choices, no message, no finish_reason)
                    // are upstream hiccups; burn a retry instead of handing
                    // the agent an empty turn.
                    Err(SlagError::ProviderTransient(why)) => {
                        last_err = format!("empty 200: {why}");
                        match self.plan_retry(None, None, None, attempts_made, budget_used, unattended_waited, req.retry) {
                            Some(plan) => {
                                next = Some(plan);
                                continue;
                            }
                            None => break,
                        }
                    }
                    Ok(mut resp) => {
                        self.attribute(&mut resp, &req).await;
                        return Ok(resp);
                    }
                    other => return other,
                }
            }

            let retry_after_header = parse_retry_after(resp.headers());
            let reset_wait = parse_ratelimit_reset(resp.headers(), now_epoch_ms());
            let code = status.as_u16();
            let text = resp.text().await.unwrap_or_default();
            let excerpt = excerpt(&text);
            if retry_hint.unwrap_or_else(|| transient_status(status)) {
                // Any transient status may carry Retry-After — 429 rate
                // limits and 502/503 gateway drains alike — so honor it
                // whenever the server sent one, not only on 429.
                last_err = format!("{status}: {excerpt}");
                match self.plan_retry(
                    Some(code),
                    retry_after_header,
                    reset_wait,
                    attempts_made,
                    budget_used,
                    unattended_waited,
                    req.retry,
                ) {
                    Some(plan) => {
                        next = Some(plan);
                        continue;
                    }
                    None => break,
                }
            }
            // Auth and billing failures have a fix; a bare status line does
            // not tell the user what it is.
            let remedy = match code {
                401 | 403 => Some(
                    "OpenRouter rejected the key. Run `slag key` to set a new one \
                     (a shell OPENROUTER_API_KEY overrides the saved key)",
                ),
                402 => Some("OpenRouter is out of credit. Top up at https://openrouter.ai/credits"),
                _ => None,
            };
            // Classified once, here; the category label leads the Display
            // string, so the dashboard's Error event reads "credit balance
            // low" instead of a raw body excerpt.
            return Err(SlagError::ProviderApi(ProviderApiError {
                status: Some(code),
                category: classify_status(code),
                retryable: false,
                excerpt: match remedy {
                    Some(remedy) => format!("{remedy} [{status}: {excerpt}]"),
                    None => excerpt,
                },
            }));
        }

        Err(SlagError::ProviderTransient(format!(
            "gave up after {attempts_made} attempts: {last_err}"
        )))
    }
}

/// One planned retry: the wait, which failure it answers, whether it
/// consumes the bounded attempt budget, and whether the wait emits
/// chunked heartbeats.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RetryPlan {
    delay: Duration,
    /// Status the wait is attributed to in heartbeats (0 = network).
    status: u16,
    free: bool,
    heartbeats: bool,
}

/// Slice a wait into 30s heartbeat chunks: (remaining secs at emit,
/// slice to sleep). A zero wait still yields one slice, so every
/// unattended retry produces at least one heartbeat.
fn heartbeat_slices(total: Duration) -> Vec<(u64, Duration)> {
    let mut out = Vec::new();
    let mut remaining = total;
    loop {
        let slice = remaining.min(HEARTBEAT_SLICE);
        out.push((remaining.as_secs(), slice));
        remaining = remaining.saturating_sub(slice);
        if remaining.is_zero() {
            return out;
        }
    }
}

/// Provider error category from an HTTP status, decided exactly once.
fn classify_status(status: u16) -> ProviderErrorCategory {
    match status {
        401 | 403 => ProviderErrorCategory::Auth,
        402 => ProviderErrorCategory::Billing,
        429 => ProviderErrorCategory::RateLimit,
        503 | 529 => ProviderErrorCategory::Overloaded,
        500..=599 => ProviderErrorCategory::Server,
        _ => ProviderErrorCategory::InvalidRequest,
    }
}

/// Stale-connection detection: the pooled connection died underneath the
/// request (connect failure, or a reset/abort/broken-pipe anywhere in
/// the error chain). These heal on a fresh client, not on a re-send.
fn is_stale_connection(e: &reqwest::Error) -> bool {
    if e.is_connect() {
        return true;
    }
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) {
                return true;
            }
        }
        source = std::error::Error::source(err);
    }
    false
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// `X-RateLimit-Reset` (unix epoch milliseconds, OpenRouter's shape) as
/// a wait-until-reset duration, capped at an hour. Past timestamps and
/// unparseable values fall back to the computed backoff — waiting until
/// the reset beats polling the limit every few seconds.
fn parse_ratelimit_reset(headers: &reqwest::header::HeaderMap, now_ms: u64) -> Option<Duration> {
    let reset: u64 = headers
        .get("x-ratelimit-reset")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if reset <= now_ms {
        return None;
    }
    Some(Duration::from_millis(reset - now_ms).min(UNATTENDED_WAIT_CEILING))
}

/// Statuses worth another attempt: timeouts (408), races (409), rate
/// limits (429), and anything 5xx.
fn transient_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error()
}

/// `x-should-retry: true|false` lets the server override the status-based
/// classification (the contract the Anthropic SDK honors).
fn should_retry_override(headers: &reqwest::header::HeaderMap) -> Option<bool> {
    match headers.get("x-should-retry")?.to_str().ok()?.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Retry-After in delta-seconds, capped at 60s. The HTTP-date form is rare
/// enough upstream that the seconds form suffices; unparseable values fall
/// back to the computed backoff.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs.min(RETRY_AFTER_CAP_SECS)))
}

/// Process-wide LCG state for retry jitter — enough randomness to spread
/// parallel anvils without pulling in a rand dependency.
static JITTER_STATE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0x2545_F491_4F6C_DD1D);

/// 0..=25, a fresh percentage per call (Knuth MMIX LCG constants).
fn next_jitter_percent() -> u64 {
    use std::sync::atomic::Ordering;
    let step = |s: u64| {
        s.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
    };
    let prev = JITTER_STATE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |s| Some(step(s)))
        .expect("closure always returns Some");
    // High bits of the advanced state; low LCG bits cycle too predictably.
    (step(prev) >> 33) % 26
}

/// Delay before retry `retry` (1-based): 500ms doubling capped at 32s,
/// plus 0-25% jitter so parallel anvils do not stampede in lockstep.
fn backoff_delay(retry: usize) -> Duration {
    backoff_delay_capped(retry, BACKOFF_CAP_MS)
}

/// The same 500ms-doubling curve under an arbitrary cap (unattended mode
/// caps at 5 minutes instead of 32s).
fn backoff_delay_capped(retry: usize, cap_ms: u64) -> Duration {
    // 500 << 10 already clears every cap in use; larger shifts would
    // only overflow.
    let shift = retry.saturating_sub(1).min(10) as u32;
    let base = (BACKOFF_BASE_MS << shift).min(cap_ms);
    Duration::from_millis(base + base * next_jitter_percent() / 100)
}

impl Provider for OpenRouter {
    fn chat(
        &self,
        req: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NormalizedResponse, SlagError>> + Send + '_>> {
        Box::pin(async move { self.chat_impl(req).await })
    }

    /// Wire the heartbeat sink: `ApiRetry` events flow here during
    /// unattended waits.
    fn set_event_sink(&self, tx: EventTx) {
        *self.events.lock().unwrap() = Some(tx);
    }

    /// Wire the cancel flag: retry waits abort when it goes up.
    fn set_cancel_flag(&self, f: CancelFlag) {
        *self.cancel.lock().unwrap() = Some(f);
    }
}

/// Account balance from `GET {base}/credits` (item 36). Both fields are
/// cumulative USD, so what is left is `total_credits - total_usage`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Credits {
    pub granted: f64,
    pub used: f64,
}

impl Credits {
    pub fn remaining(&self) -> f64 {
        self.granted - self.used
    }
}

/// Parse the `/credits` body. Split out from the fetch so the shape is
/// testable without a server; `None` on any shape slag does not recognize.
pub fn parse_credits(body: &str) -> Option<Credits> {
    let root: Value = serde_json::from_str(body).ok()?;
    let data = root.get("data").unwrap_or(&root);
    let num = |k: &str| data.get(k).and_then(|v| v.as_f64());
    Some(Credits {
        granted: num("total_credits")?,
        used: num("total_usage")?,
    })
}

/// Account balance, or `None` when the endpoint is unreachable or the key
/// is refused. A balance readout never blocks a forge.
pub async fn fetch_credits(api_key: &str, base_url: &str) -> Option<Credits> {
    let url = format!("{}/credits", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .bearer_auth(api_key)
        .header("HTTP-Referer", "https://slag.dev")
        .header("X-Title", "slag")
        .send()
        .await
        .ok()?;
    resp.status().is_success().then_some(())?;
    parse_credits(&resp.text().await.ok()?)
}

/// Outcome of a key check. Onboarding treats these differently: a key
/// OpenRouter refused is worth retyping, an unreachable OpenRouter is not.
pub enum KeyCheck {
    Valid,
    Rejected(String),
    Unreachable(String),
}

/// Cheap key check: GET {base}/key, which reports the bearer token's own
/// limits and so requires auth. `/models` is public — it answers 200 for a
/// bogus key and even for no key at all, which made checking it worthless.
pub async fn check_key(api_key: &str, base_url: &str) -> KeyCheck {
    let url = format!("{}/key", base_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(e) => return KeyCheck::Unreachable(format!("client build failed: {e}")),
    };
    let resp = client
        .get(&url)
        .bearer_auth(api_key)
        .header("HTTP-Referer", "https://slag.dev")
        .header("X-Title", "slag")
        .send()
        .await;

    match resp {
        Ok(resp) if resp.status().is_success() => KeyCheck::Valid,
        // 4xx is a verdict on the key; 5xx is OpenRouter having a bad day.
        Ok(resp) if resp.status().is_client_error() => {
            KeyCheck::Rejected(resp.status().to_string())
        }
        Ok(resp) => KeyCheck::Unreachable(resp.status().to_string()),
        Err(e) => KeyCheck::Unreachable(format!("{e}")),
    }
}

/// Boolean form of `check_key` for callers that treat every failure alike.
pub async fn validate_key(api_key: &str, base_url: &str) -> Result<(), SlagError> {
    match check_key(api_key, base_url).await {
        KeyCheck::Valid => Ok(()),
        KeyCheck::Rejected(why) | KeyCheck::Unreachable(why) => {
            Err(SlagError::Provider(format!("key validation failed: {why}")))
        }
    }
}

/// Context window (tokens) for `model` from GET {base}/models. The
/// endpoint is public — no auth needed. A variant suffix falls back to the
/// bare id (`qwen/qwen3-coder:free` → `qwen/qwen3-coder`) since variants
/// share the base model's window. `None` on any failure.
pub async fn fetch_context_length(base_url: &str, model: &str) -> Option<u64> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("HTTP-Referer", "https://slag.dev")
        .header("X-Title", "slag")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let models: ModelsResponse = resp.json().await.ok()?;
    let bare = model.split(':').next().unwrap_or(model);
    models
        .data
        .iter()
        .find(|m| m.id == model)
        .or_else(|| models.data.iter().find(|m| m.id == bare))
        .and_then(|m| m.context_length)
}

/// Raw `GET {base}/models` body. The endpoint is public — no auth needed.
/// `None` on any failure; both the window cache and the price table treat
/// that as "unknown", never as an error.
pub async fn fetch_models_body(base_url: &str) -> Option<String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("HTTP-Referer", "https://slag.dev")
        .header("X-Title", "slag")
        .send()
        .await
        .ok()?;
    resp.status().is_success().then_some(())?;
    resp.text().await.ok()
}

/// Everything one `/models` fetch yields: context windows and prices.
#[derive(Debug, Default)]
struct ModelsIndex {
    windows: std::collections::HashMap<String, u64>,
    prices: crate::engine::pricing::PricingTable,
}

impl ModelsIndex {
    fn parse(body: Option<&str>) -> Self {
        let Some(body) = body else {
            return Self::default();
        };
        let windows = serde_json::from_str::<ModelsResponse>(body)
            .map(|m| {
                m.data
                    .into_iter()
                    .filter_map(|e| e.context_length.map(|c| (e.id, c)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            windows,
            prices: crate::engine::pricing::parse_table(body),
        }
    }

    /// A variant suffix falls back to the bare id
    /// (`qwen/qwen3-coder:free` → `qwen/qwen3-coder`), since variants share
    /// the base model's window.
    fn window(&self, model: &str) -> Option<u64> {
        if let Some(w) = self.windows.get(model) {
            return Some(*w);
        }
        let bare = model.split(':').next().unwrap_or(model);
        self.windows.get(bare).copied()
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    context_length: Option<u64>,
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

/// Distinguishes synthesized fallback tool-call ids across responses so a
/// multi-turn history never replays duplicate ids to strict backends.
static FALLBACK_ID_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn normalize(api: ApiResponse) -> Result<NormalizedResponse, SlagError> {
    // A 200 with no choices, no message, or no finish_reason is an upstream
    // hiccup, not a completion — classify transient so the caller retries.
    let choice = api
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| SlagError::ProviderTransient("no choices in response".into()))?;
    let msg = choice
        .message
        .ok_or_else(|| SlagError::ProviderTransient("choice missing message".into()))?;
    let finish = choice
        .finish_reason
        .ok_or_else(|| SlagError::ProviderTransient("choice missing finish_reason".into()))?;

    let seq = FALLBACK_ID_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tool_calls: Vec<ToolCall> = msg
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, tc)| ToolCall {
            // Some upstreams omit ids; strict backends reject "" on replay.
            id: if tc.id.is_empty() { format!("call_{seq}_{i}") } else { tc.id },
            name: tc.function.name,
            arguments: tc.function.arguments,
        })
        .collect();

    let finish_reason = match finish.as_str() {
        "stop" => FinishReason::Stop,
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        _ => FinishReason::Other,
    };

    let reasoning = msg
        .reasoning
        .filter(|s| !s.is_empty())
        .or_else(|| reasoning_from_details(msg.reasoning_details.as_ref()));

    Ok(NormalizedResponse {
        model: api.model.filter(|m| !m.is_empty()),
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
    /// Which model actually answered. With `openrouter/auto` this is the
    /// only place the routed id appears.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct ApiChoice {
    #[serde(default)]
    message: Option<ApiMessage>,
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

    /// Provider with the env-derived knobs pinned, so concurrent tests
    /// mutating `SLAG_MODEL_FALLBACK` / `SLAG_UNATTENDED_RETRY` cannot
    /// leak into body and retry assertions.
    fn pinned(base_url: &str) -> OpenRouter {
        OpenRouter::with_base_url("sk-test", base_url)
            .with_fallback(None)
            .with_unattended(false)
    }

    fn body_builder() -> OpenRouter {
        pinned("http://localhost:0")
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
            role: crate::engine::Role::Smith,
            retry: RetryPolicy::full(),
        }
    }

    #[test]
    fn fallback_tool_call_ids_stay_unique_across_responses() {
        let api = || -> ApiResponse {
            serde_json::from_value(json!({
                "choices": [{
                    "message": {
                        "tool_calls": [
                            { "id": "", "function": { "name": "bash", "arguments": "{}" } },
                            { "id": "", "function": { "name": "grep", "arguments": "{}" } },
                        ]
                    },
                    "finish_reason": "tool_calls"
                }]
            }))
            .unwrap()
        };
        let first = normalize(api()).unwrap();
        let second = normalize(api()).unwrap();
        // Within one response the ids differ by index; across responses the
        // sequence counter keeps them from colliding in the replayed history.
        assert_ne!(first.tool_calls[0].id, first.tool_calls[1].id);
        assert_ne!(first.tool_calls[0].id, second.tool_calls[0].id);
        assert_ne!(first.tool_calls[1].id, second.tool_calls[1].id);
    }

    #[test]
    fn body_wraps_tools_and_sets_tool_choice() {
        let body = body_builder().build_body(&request(vec![spec()], Some(Effort::High)));
        assert_eq!(body["tool_choice"], json!("auto"));
        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("read_file"));
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        assert_eq!(body["usage"], json!({ "include": true }));
    }

    #[test]
    fn body_omits_tool_choice_and_reasoning_when_absent() {
        let body = body_builder().build_body(&request(vec![], None));
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
        let body = body_builder().build_body(&req);
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
        let body = body_builder().build_body(&req);
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
        let body = body_builder().build_body(&req);
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
        // Isolate the config dir: the mock serves no /models, so the price
        // lookup falls back to the on-disk cache, and the real one would
        // hand a costless response a cost.
        let (_env, _dir) = crate::config::isolated_config_dir();
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
            .and(path("/key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .mount(&server)
            .await;

        validate_key("sk-test", &server.uri())
            .await
            .expect("valid key");

        let bad = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/key"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&bad)
            .await;

        let err = validate_key("sk-bad", &bad.uri())
            .await
            .expect_err("bad key");
        assert!(err.to_string().contains("401"));
    }

    /// A bare "401 Unauthorized" reads like a slag bug. Auth and billing
    /// failures are the two the user can actually fix, so they carry the fix.
    #[tokio::test]
    async fn auth_and_billing_failures_carry_a_remedy() {
        for (status, needle) in [(401, "slag key"), (403, "slag key"), (402, "credits")] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;

            let err = OpenRouter::with_base_url("sk-bad", &server.uri())
                .chat(request(vec![], None))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains(needle), "{status} said: {err}");
            // The raw status stays, so bug reports keep the detail.
            assert!(err.contains(&status.to_string()), "{status} said: {err}");
        }
    }

    /// `/models` answers 200 for any bearer token, so checking it would
    /// wave every typo through. `/key` is the endpoint that needs auth.
    #[tokio::test]
    async fn check_key_asks_the_authenticated_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/key"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        assert!(matches!(
            check_key("sk-bad", &server.uri()).await,
            KeyCheck::Rejected(_)
        ));
        // A 5xx is OpenRouter's problem, not the key's.
        let flaky = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/key"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&flaky)
            .await;
        assert!(matches!(
            check_key("sk-fine", &flaky.uri()).await,
            KeyCheck::Unreachable(_)
        ));
    }

    fn ok_body() -> Value {
        json!({
            "choices": [{
                "message": { "content": "forged" },
                "finish_reason": "stop",
            }],
        })
    }

    #[test]
    fn backoff_doubles_caps_and_jitters_within_bounds() {
        for retry in 1..=10usize {
            let base = (BACKOFF_BASE_MS << (retry - 1).min(63)).min(BACKOFF_CAP_MS);
            let ms = backoff_delay(retry).as_millis() as u64;
            assert!(ms >= base, "retry {retry}: {ms} below base {base}");
            assert!(ms <= base + base / 4, "retry {retry}: {ms} above base+25%");
        }
        // Doubling stops at the cap.
        assert!(backoff_delay(7).as_millis() as u64 <= BACKOFF_CAP_MS + BACKOFF_CAP_MS / 4);
        assert!(backoff_delay(63).as_millis() as u64 >= BACKOFF_CAP_MS);
    }

    #[test]
    fn jitter_stays_in_percent_range_and_varies() {
        let draws: Vec<u64> = (0..100).map(|_| next_jitter_percent()).collect();
        assert!(draws.iter().all(|&p| p <= 25));
        // An LCG that returns one constant would defeat the point.
        assert!(draws.iter().collect::<std::collections::HashSet<_>>().len() > 1);
    }

    #[test]
    fn retry_after_parses_seconds_and_caps_at_60() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
        headers.insert("retry-after", "3".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(3)));
        headers.insert("retry-after", "120".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(60)));
        // HTTP-date (unsupported form) falls back to computed backoff.
        headers.insert("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn should_retry_header_parses_true_false_only() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(should_retry_override(&headers), None);
        headers.insert("x-should-retry", "true".parse().unwrap());
        assert_eq!(should_retry_override(&headers), Some(true));
        headers.insert("x-should-retry", "false".parse().unwrap());
        assert_eq!(should_retry_override(&headers), Some(false));
        headers.insert("x-should-retry", "maybe".parse().unwrap());
        assert_eq!(should_retry_override(&headers), None);
    }

    #[test]
    fn transient_statuses_cover_408_409_429_and_5xx() {
        for code in [408u16, 409, 429, 500, 502, 503, 529] {
            assert!(transient_status(reqwest::StatusCode::from_u16(code).unwrap()), "{code}");
        }
        for code in [400u16, 401, 402, 403, 404, 422] {
            assert!(!transient_status(reqwest::StatusCode::from_u16(code).unwrap()), "{code}");
        }
    }

    #[test]
    fn normalize_classifies_incomplete_200s_as_transient() {
        let cases = [
            json!({ "choices": [] }),
            json!({ "choices": [{ "finish_reason": "stop" }] }),
            json!({ "choices": [{ "message": { "content": "hi" } }] }),
        ];
        for body in cases {
            let api: ApiResponse = serde_json::from_value(body.clone()).unwrap();
            let err = normalize(api).expect_err("incomplete 200 must err");
            assert!(err.retryable(), "{body} gave permanent: {err}");
        }
    }

    #[tokio::test]
    async fn retry_after_overrides_computed_backoff() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "1")
                    .set_body_string("slow down"),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let started = std::time::Instant::now();
        let resp = provider.chat(request(vec![], None)).await.expect("retry ok");
        assert_eq!(resp.content, "forged");
        // Computed first-retry backoff tops out at 625ms; only the header
        // explains a full second of waiting.
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "waited only {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn retries_408_and_409_then_succeeds() {
        for status in [408u16, 409] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(status).set_body_string("try again"))
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
                .expect(1)
                .mount(&server)
                .await;

            let provider = OpenRouter::with_base_url("sk-test", server.uri());
            let resp = provider.chat(request(vec![], None)).await.expect("retry ok");
            assert_eq!(resp.content, "forged", "status {status}");
        }
    }

    #[tokio::test]
    async fn fails_fast_on_400_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .expect(1)
            .mount(&server)
            .await;

        let err = OpenRouter::with_base_url("sk-test", server.uri())
            .chat(request(vec![], None))
            .await
            .expect_err("400 must fail");
        assert!(!err.retryable(), "400 must be permanent: {err}");
        assert!(err.to_string().contains("400"), "message was: {err}");
    }

    #[tokio::test]
    async fn x_should_retry_true_retries_a_400() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(400)
                    .insert_header("x-should-retry", "true")
                    .set_body_string("flaky 400"),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider.chat(request(vec![], None)).await.expect("header-driven retry");
        assert_eq!(resp.content, "forged");
    }

    #[tokio::test]
    async fn x_should_retry_false_stops_a_503() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(503)
                    .insert_header("x-should-retry", "false")
                    .set_body_string("do not bother"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let err = OpenRouter::with_base_url("sk-test", server.uri())
            .chat(request(vec![], None))
            .await
            .expect_err("header forbids retry");
        assert!(!err.retryable(), "must be permanent: {err}");
        assert!(err.to_string().contains("503"), "message was: {err}");
    }

    #[tokio::test]
    async fn empty_200_retries_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let resp = provider.chat(request(vec![], None)).await.expect("empty 200 retried");
        assert_eq!(resp.content, "forged");
    }

    #[tokio::test]
    async fn persistent_empty_200_gives_up_transient() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })))
            .expect(3)
            .mount(&server)
            .await;

        let err = OpenRouter::with_base_url("sk-test", server.uri())
            .with_max_attempts(3)
            .chat(request(vec![], None))
            .await
            .expect_err("all empty");
        assert!(err.retryable(), "exhausted retries stay transient: {err}");
        assert!(err.to_string().contains("gave up after 3"), "message was: {err}");
    }

    /// Env parsing is tested through the pure helper — mutating the real
    /// environment races parallel tests.
    #[test]
    fn max_attempts_parses_env_and_defaults_to_8() {
        assert_eq!(parse_max_attempts(None), DEFAULT_MAX_ATTEMPTS);
        assert_eq!(parse_max_attempts(Some("10".into())), 10);
        assert_eq!(parse_max_attempts(Some(" 5 ".into())), 5);
        // Zero and garbage fall back rather than disabling retries.
        assert_eq!(parse_max_attempts(Some("0".into())), DEFAULT_MAX_ATTEMPTS);
        assert_eq!(parse_max_attempts(Some("lots".into())), DEFAULT_MAX_ATTEMPTS);
    }

    #[tokio::test]
    async fn configured_retry_budget_bounds_the_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })))
            .expect(2)
            .mount(&server)
            .await;

        let err = OpenRouter::with_base_url("sk-test", server.uri())
            .with_max_attempts(2)
            .chat(request(vec![], None))
            .await
            .expect_err("budget of 2 exhausted");
        assert!(err.to_string().contains("gave up after 2"), "message was: {err}");
    }

    /// Item 50: a side call takes one swing. The provider budget says
    /// eight, but a judge or summary request carries `RetryPolicy::side()`
    /// and must not multiply load across every anvil during a capacity
    /// event.
    #[tokio::test]
    async fn a_side_policy_request_takes_one_swing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(529))
            .expect(1)
            .mount(&server)
            .await;

        let mut req = request(vec![], None);
        req.retry = RetryPolicy::side();
        let err = OpenRouter::with_base_url("sk-test", server.uri())
            .chat(req)
            .await
            .expect_err("529 with a one-swing policy");
        assert!(err.to_string().contains("gave up after 1"), "message was: {err}");
    }

    /// The same request under the default policy rides the provider-wide
    /// budget, so the side policy is doing the bounding, not the endpoint.
    #[tokio::test]
    async fn a_full_policy_request_rides_the_provider_budget() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(529))
            .expect(3)
            .mount(&server)
            .await;

        let err = OpenRouter::with_base_url("sk-test", server.uri())
            .with_max_attempts(3)
            .chat(request(vec![], None))
            .await
            .expect_err("budget of 3 exhausted");
        assert!(err.to_string().contains("gave up after 3"), "message was: {err}");
    }

    /// Unattended mode makes capacity errors free — but only for calls
    /// that asked to be persistent. A side call opts out, so it stops at
    /// its one swing instead of waiting out the rate limit.
    #[tokio::test]
    async fn a_side_policy_call_does_not_wait_out_a_capacity_event_unattended() {
        let provider = pinned("http://localhost:0").with_unattended(true);
        assert!(
            provider
                .plan_retry(
                    Some(529),
                    None,
                    None,
                    1,
                    1,
                    Duration::ZERO,
                    RetryPolicy::side()
                )
                .is_none(),
            "a side call must not take the free unattended retry"
        );
        assert!(
            provider
                .plan_retry(
                    Some(529),
                    None,
                    None,
                    1,
                    1,
                    Duration::ZERO,
                    RetryPolicy::full()
                )
                .is_some_and(|p| p.free),
            "a full call still gets the free retry"
        );
    }

    /// Item 34: OpenRouter omits `usage.cost` behind most proxies. The
    /// local table fills it in and the number is marked as an estimate, so
    /// budget caps keep binding without the readout claiming the provider
    /// said so. Item 35: the same response carries its ledger key.
    #[tokio::test]
    async fn a_missing_cost_is_filled_from_the_price_table_and_flagged() {
        let cfg = tempfile::tempdir().expect("tempdir");
        // Point the pricing cache at a scratch dir: a real ~/.config/slag
        // cache would answer before the mock /models ever gets hit.
        std::env::set_var("SLAG_CONFIG_DIR", cfg.path());

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "qwen/qwen3-coder",
                    "pricing": { "prompt": "0.000001", "completion": "0.000002" },
                }],
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "qwen/qwen3-coder",
                "choices": [{
                    "message": { "role": "assistant", "content": "forged" },
                    "finish_reason": "stop",
                }],
                "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 },
            })))
            .mount(&server)
            .await;

        let resp = OpenRouter::with_base_url("sk-test", server.uri())
            .chat(request(vec![], None))
            .await
            .expect("chat ok");

        // 1000 * 1e-6 + 500 * 2e-6 = $0.002
        let cost = resp.usage.cost.expect("cost estimated from the table");
        assert!((cost - 0.002).abs() < 1e-9, "got {cost}");
        assert!(resp.usage.estimated, "a local estimate must say so");
        assert_eq!(resp.usage.model.as_deref(), Some("qwen/qwen3-coder"));
        assert_eq!(resp.usage.role, Some(crate::engine::Role::Smith));

        std::env::remove_var("SLAG_CONFIG_DIR");
    }

    /// A cost the provider actually reported is never overwritten, and is
    /// never marked as an estimate.
    #[tokio::test]
    async fn a_provider_reported_cost_stays_exact() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "qwen/qwen3-coder",
                "choices": [{
                    "message": { "role": "assistant", "content": "forged" },
                    "finish_reason": "stop",
                }],
                "usage": {
                    "prompt_tokens": 1000, "completion_tokens": 500,
                    "total_tokens": 1500, "cost": 0.5,
                },
            })))
            .mount(&server)
            .await;

        let resp = OpenRouter::with_base_url("sk-test", server.uri())
            .chat(request(vec![], None))
            .await
            .expect("chat ok");
        assert_eq!(resp.usage.cost, Some(0.5));
        assert!(!resp.usage.estimated);
    }

    /// Item 35 promises the assay splits judge and duel spend out of the
    /// session total. The judge and the summarizer call the provider
    /// straight, never through `ForgeAgent`, so the ledger has to be fed
    /// where every response comes back or their rows silently go missing.
    #[tokio::test]
    async fn every_call_site_reaches_the_ledger_not_just_the_smith() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "ledger/judge-probe",
                "choices": [{
                    "message": { "role": "assistant", "content": "scored" },
                    "finish_reason": "stop",
                }],
                "usage": {
                    "prompt_tokens": 700, "completion_tokens": 300,
                    "total_tokens": 1000, "cost": 0.25,
                },
            })))
            .mount(&server)
            .await;

        let mut req = request(vec![], None);
        req.role = crate::engine::Role::Judge;
        OpenRouter::with_base_url("sk-test", server.uri())
            .chat(req)
            .await
            .expect("chat ok");

        // A unique model id keeps this assertion independent of whatever
        // else the parallel test run folded into the shared ledger.
        let row = crate::engine::stats::snapshot()
            .ledger
            .rows()
            .into_iter()
            .find(|r| r.model == "ledger/judge-probe")
            .expect("a judge call must appear on the ledger");
        assert_eq!(row.role, crate::engine::Role::Judge);
        assert!((row.cost() - 0.25).abs() < 1e-9, "got {}", row.cost());
    }

    #[test]
    fn parse_credits_reads_the_data_envelope_and_the_bare_shape() {
        let wrapped = parse_credits(r#"{"data":{"total_credits":20.0,"total_usage":1.69}}"#)
            .expect("wrapped shape");
        assert!((wrapped.remaining() - 18.31).abs() < 1e-9, "got {}", wrapped.remaining());
        // Some proxies drop the envelope.
        let bare = parse_credits(r#"{"total_credits":5.0,"total_usage":5.0}"#).expect("bare shape");
        assert_eq!(bare.remaining(), 0.0);
        // An unrecognized shape reads as "unknown", never as zero balance.
        assert_eq!(parse_credits(r#"{"data":{"balance":3}}"#), None);
        assert_eq!(parse_credits("nonsense"), None);
    }

    fn models_body() -> Value {
        json!({
            "data": [
                { "id": "qwen/qwen3-coder", "context_length": 262144 },
                { "id": "mystery/model" },
            ],
        })
    }

    #[tokio::test]
    async fn fetch_context_length_matches_exact_and_variant_ids() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body()))
            .mount(&server)
            .await;

        assert_eq!(
            fetch_context_length(&server.uri(), "qwen/qwen3-coder").await,
            Some(262144)
        );
        // Variant suffix falls back to the bare id.
        assert_eq!(
            fetch_context_length(&server.uri(), "qwen/qwen3-coder:free").await,
            Some(262144)
        );
        // Listed but windowless, and unlisted, both come back None.
        assert_eq!(fetch_context_length(&server.uri(), "mystery/model").await, None);
        assert_eq!(fetch_context_length(&server.uri(), "no/such-model").await, None);
    }

    #[tokio::test]
    async fn context_length_caches_per_model() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        assert_eq!(provider.context_length("qwen/qwen3-coder").await, Some(262144));
        // Second lookup answers from the cache — the mock allows one GET.
        assert_eq!(provider.context_length("qwen/qwen3-coder").await, Some(262144));
    }

    #[tokio::test]
    async fn retry_after_is_honored_on_503_too() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(503)
                    .insert_header("retry-after", "1")
                    .set_body_string("draining"),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_base_url("sk-test", server.uri());
        let started = std::time::Instant::now();
        let resp = provider.chat(request(vec![], None)).await.expect("retry ok");
        assert_eq!(resp.content, "forged");
        // Computed first-retry backoff tops out at 625ms; only the header
        // explains a full second of waiting on a gateway 503.
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "waited only {:?}",
            started.elapsed()
        );
    }

    // ─── Item 53: configurable timeout + stale-connection rebuild ───

    #[test]
    fn timeout_parses_env_and_defaults_to_300s() {
        assert_eq!(parse_timeout_ms(None), Duration::from_millis(300_000));
        assert_eq!(parse_timeout_ms(Some("5000".into())), Duration::from_millis(5000));
        assert_eq!(parse_timeout_ms(Some(" 1500 ".into())), Duration::from_millis(1500));
        // Zero and garbage fall back rather than disabling the timeout.
        assert_eq!(parse_timeout_ms(Some("0".into())), Duration::from_millis(300_000));
        assert_eq!(parse_timeout_ms(Some("forever".into())), Duration::from_millis(300_000));
    }

    #[tokio::test]
    async fn configured_timeout_cuts_a_hung_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(ok_body())
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&server)
            .await;

        let err = pinned(&server.uri())
            .with_timeout(Duration::from_millis(50))
            .with_max_attempts(1)
            .chat(request(vec![], None))
            .await
            .expect_err("timeout must cut the request");
        assert!(err.retryable(), "timeouts are transient: {err}");
        assert!(err.to_string().contains("request failed"), "{err}");
    }

    #[tokio::test]
    async fn connect_refused_classifies_as_stale_connection() {
        // Bind then drop: the port is guaranteed dead.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let err = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect_err("nothing listens there");
        assert!(is_stale_connection(&err), "{err}");
    }

    /// A dead endpoint exercises the rebuild path (fresh client with no
    /// idle pool) on every retry; the outcome stays a clean transient.
    #[tokio::test]
    async fn dead_endpoint_rebuilds_the_client_and_gives_up_transient() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let err = pinned(&format!("http://127.0.0.1:{port}"))
            .with_max_attempts(2)
            .chat(request(vec![], None))
            .await
            .expect_err("dead endpoint");
        assert!(err.retryable(), "{err}");
        assert!(err.to_string().contains("gave up after 2"), "{err}");
    }

    // ─── Item 48: native fallback-model routing ───

    #[test]
    fn build_body_sends_the_native_fallback_routing_array() {
        let body = body_builder()
            .with_fallback(Some("deepseek/deepseek-chat"))
            .build_body(&request(vec![], None));
        assert_eq!(
            body["models"],
            json!(["qwen/qwen3-coder", "deepseek/deepseek-chat"])
        );
        // The plain model field stays for older proxies.
        assert_eq!(body["model"], json!("qwen/qwen3-coder"));

        // No fallback, or a fallback equal to the primary: no array.
        let body = body_builder().build_body(&request(vec![], None));
        assert!(body.get("models").is_none());
        let body = body_builder()
            .with_fallback(Some("qwen/qwen3-coder"))
            .build_body(&request(vec![], None));
        assert!(body.get("models").is_none(), "self-fallback is pointless");
    }

    #[tokio::test]
    async fn fallback_routing_array_goes_on_the_wire() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({
                "model": "qwen/qwen3-coder",
                "models": ["qwen/qwen3-coder", "deepseek/deepseek-chat"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;

        let resp = pinned(&server.uri())
            .with_fallback(Some("deepseek/deepseek-chat"))
            .chat(request(vec![], None))
            .await
            .expect("chat ok");
        assert_eq!(resp.content, "forged");
    }

    // ─── Item 49: unattended persistent retry + heartbeats ───

    #[tokio::test]
    async fn unattended_capacity_errors_retry_past_the_budget_with_heartbeats() {
        for status in [429u16, 529] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("retry-after", "0")
                        .set_body_string("capacity"),
                )
                .up_to_n_times(3)
                .expect(3)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
                .expect(1)
                .mount(&server)
                .await;

            let provider = pinned(&server.uri())
                .with_max_attempts(1)
                .with_unattended(true);
            let (tx, mut rx) = crate::engine::events::channel();
            provider.set_event_sink(tx);

            let resp = provider.chat(request(vec![], None)).await.expect("outlasted the limit");
            assert_eq!(resp.content, "forged", "status {status}");

            let mut heartbeats = 0;
            while let Ok(event) = rx.try_recv() {
                let EngineEvent::ApiRetry { attempt, status: s, remaining_secs } = event else {
                    panic!("unexpected event");
                };
                assert_eq!(s, status);
                assert!(attempt >= 1 && attempt <= 3, "attempt {attempt}");
                // Regression: `Retry-After: 0` used to plan zero-delay
                // retries — an unbounded request storm. Every free retry
                // now waits a real, floored delay.
                assert!(remaining_secs >= 1, "zero-delay retry storm is back");
                heartbeats += 1;
            }
            // One heartbeat per free retry, even for zero-length waits.
            assert_eq!(heartbeats, 3, "status {status}");
        }
    }

    /// Unattended mode frees only capacity errors. Everything else keeps
    /// the bounded budget — a broken request must still fail.
    #[tokio::test]
    async fn unattended_leaves_non_capacity_errors_bounded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(2)
            .mount(&server)
            .await;

        let err = pinned(&server.uri())
            .with_max_attempts(2)
            .with_unattended(true)
            .chat(request(vec![], None))
            .await
            .expect_err("500 stays bounded");
        assert!(err.to_string().contains("gave up after 2"), "{err}");
    }

    #[test]
    fn plan_retry_prefers_reset_over_retry_after_and_caps_waits() {
        let provider = pinned("http://localhost:0").with_unattended(true).with_max_attempts(1);

        // Reset timestamp wins over Retry-After: wait until the window
        // opens instead of polling it.
        let plan = provider
            .plan_retry(
                Some(429),
                Some(Duration::from_secs(3)),
                Some(Duration::from_secs(120)),
                1,
                1,
                Duration::ZERO,
                RetryPolicy::full(),
            )
            .expect("free retry");
        assert_eq!(plan.delay, Duration::from_secs(120));
        assert!(plan.free && plan.heartbeats);
        assert_eq!(plan.status, 429);

        // No reset: Retry-After.
        let plan = provider
            .plan_retry(Some(429), Some(Duration::from_secs(3)), None, 1, 1, Duration::ZERO, RetryPolicy::full())
            .unwrap();
        assert_eq!(plan.delay, Duration::from_secs(3));

        // Neither: computed backoff, capped at 5 minutes (+25% jitter).
        let plan = provider
            .plan_retry(Some(529), None, None, 30, 1, Duration::ZERO, RetryPolicy::full())
            .unwrap();
        assert!(plan.delay >= Duration::from_secs(300), "{:?}", plan.delay);
        assert!(plan.delay <= Duration::from_secs(375), "{:?}", plan.delay);

        // A reset hours away is clamped to the one-hour ceiling.
        let plan = provider
            .plan_retry(Some(429), None, Some(Duration::from_secs(2 * 3600)), 1, 1, Duration::ZERO, RetryPolicy::full())
            .unwrap();
        assert_eq!(plan.delay, UNATTENDED_WAIT_CEILING);
    }

    /// Regression: a server-controlled `Retry-After: 0` (or a reset
    /// timestamp ms ahead) used to plan `Duration::ZERO` waits forever —
    /// `unattended_waited` never grew, the 6h ceiling never fired, and the
    /// loop hammered the API at full speed. Every capacity wait is floored.
    #[test]
    fn plan_retry_floors_zero_and_near_zero_capacity_waits() {
        let provider = pinned("http://localhost:0").with_unattended(true).with_max_attempts(1);

        let plan = provider
            .plan_retry(Some(429), Some(Duration::ZERO), None, 1, 1, Duration::ZERO, RetryPolicy::full())
            .expect("free retry");
        assert!(plan.delay >= UNATTENDED_MIN_DELAY, "{:?}", plan.delay);

        let plan = provider
            .plan_retry(Some(429), None, Some(Duration::from_millis(1)), 1, 1, Duration::ZERO, RetryPolicy::full())
            .expect("free retry");
        assert!(plan.delay >= UNATTENDED_MIN_DELAY, "{:?}", plan.delay);

        // The floor keeps the ceiling reachable: at the edge, stop.
        let waited = UNATTENDED_TOTAL_CEILING - Duration::from_millis(500);
        assert!(
            provider.plan_retry(Some(429), Some(Duration::ZERO), None, 1, 1, waited, RetryPolicy::full()).is_none(),
            "floored delay must trip the cumulative ceiling"
        );
    }

    /// Regression: the cancel flag was invisible inside retry waits — a
    /// Ctrl-C left an unattended forge sleeping and re-requesting for up
    /// to 6 hours. A raised flag now aborts before the next request.
    #[tokio::test]
    async fn cancel_flag_aborts_a_retry_wait_before_the_next_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("limited"))
            .expect(1) // cancelled during the first wait: no second request
            .mount(&server)
            .await;

        let provider = pinned(&server.uri()).with_unattended(true);
        let cancel = crate::engine::CancelFlag::default();
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        provider.set_cancel_flag(cancel);

        let err = provider.chat(request(vec![], None)).await.expect_err("cancelled");
        assert!(matches!(err, SlagError::Cancelled), "{err}");
    }

    #[test]
    fn plan_retry_enforces_the_unattended_total_ceiling() {
        let provider = pinned("http://localhost:0").with_unattended(true).with_max_attempts(1);
        // Cumulative waiting at the ceiling: even a capacity error stops.
        let plan = provider.plan_retry(
            Some(429),
            Some(Duration::from_secs(1)),
            None,
            5,
            1,
            UNATTENDED_TOTAL_CEILING,
            RetryPolicy::full(),
        );
        assert!(plan.is_none(), "ceiling must end the wait");
    }

    #[test]
    fn plan_retry_bounded_path_respects_the_budget() {
        let provider = pinned("http://localhost:0").with_max_attempts(3);
        let plan = provider
            .plan_retry(Some(500), None, None, 1, 1, Duration::ZERO, RetryPolicy::full())
            .expect("budget left");
        assert!(!plan.free && !plan.heartbeats);
        assert!(provider.plan_retry(Some(500), None, None, 3, 3, Duration::ZERO, RetryPolicy::full()).is_none());
        // Without unattended mode a 429 is bounded like everything else.
        assert!(provider.plan_retry(Some(429), None, None, 3, 3, Duration::ZERO, RetryPolicy::full()).is_none());
        // Reset timestamps never stretch a bounded wait.
        let plan = provider
            .plan_retry(Some(429), None, Some(Duration::from_secs(600)), 1, 1, Duration::ZERO, RetryPolicy::full())
            .unwrap();
        assert!(plan.delay < Duration::from_secs(2), "{:?}", plan.delay);
    }

    #[test]
    fn ratelimit_reset_reads_epoch_ms_and_caps_at_an_hour() {
        let mut headers = reqwest::header::HeaderMap::new();
        let now = 1_700_000_000_000u64;
        assert_eq!(parse_ratelimit_reset(&headers, now), None);

        headers.insert("x-ratelimit-reset", format!("{}", now + 90_000).parse().unwrap());
        assert_eq!(parse_ratelimit_reset(&headers, now), Some(Duration::from_secs(90)));

        // Past and garbage values fall back to computed backoff.
        headers.insert("x-ratelimit-reset", format!("{}", now - 1).parse().unwrap());
        assert_eq!(parse_ratelimit_reset(&headers, now), None);
        headers.insert("x-ratelimit-reset", "soon".parse().unwrap());
        assert_eq!(parse_ratelimit_reset(&headers, now), None);

        // A reset days away is clamped to the ceiling.
        headers.insert(
            "x-ratelimit-reset",
            format!("{}", now + 48 * 3600 * 1000).parse().unwrap(),
        );
        assert_eq!(parse_ratelimit_reset(&headers, now), Some(UNATTENDED_WAIT_CEILING));
    }

    #[test]
    fn heartbeat_slices_chunk_long_waits_into_thirty_seconds() {
        assert_eq!(
            heartbeat_slices(Duration::from_secs(95)),
            vec![
                (95, Duration::from_secs(30)),
                (65, Duration::from_secs(30)),
                (35, Duration::from_secs(30)),
                (5, Duration::from_secs(5)),
            ]
        );
        assert_eq!(heartbeat_slices(Duration::from_secs(30)), vec![(30, Duration::from_secs(30))]);
        // Zero wait still emits one heartbeat.
        assert_eq!(heartbeat_slices(Duration::ZERO), vec![(0, Duration::ZERO)]);
    }

    #[test]
    fn backoff_capped_reaches_and_respects_the_five_minute_cap() {
        let ms = backoff_delay_capped(20, UNATTENDED_BACKOFF_CAP_MS).as_millis() as u64;
        assert!(ms >= UNATTENDED_BACKOFF_CAP_MS, "{ms}");
        assert!(ms <= UNATTENDED_BACKOFF_CAP_MS + UNATTENDED_BACKOFF_CAP_MS / 4, "{ms}");
    }

    // ─── Item 54: typed provider error taxonomy ───

    #[test]
    fn classify_status_covers_the_taxonomy() {
        assert_eq!(classify_status(401), ProviderErrorCategory::Auth);
        assert_eq!(classify_status(403), ProviderErrorCategory::Auth);
        assert_eq!(classify_status(402), ProviderErrorCategory::Billing);
        assert_eq!(classify_status(429), ProviderErrorCategory::RateLimit);
        assert_eq!(classify_status(503), ProviderErrorCategory::Overloaded);
        assert_eq!(classify_status(529), ProviderErrorCategory::Overloaded);
        assert_eq!(classify_status(500), ProviderErrorCategory::Server);
        assert_eq!(classify_status(400), ProviderErrorCategory::InvalidRequest);
        assert_eq!(classify_status(404), ProviderErrorCategory::InvalidRequest);
    }

    /// Permanent failures come back typed: status + category + retryable
    /// + excerpt, with the human label leading the Display string the
    /// dashboard shows.
    #[tokio::test]
    async fn permanent_failures_carry_the_typed_taxonomy() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(402).set_body_string("insufficient credits"))
            .mount(&server)
            .await;

        let err = pinned(&server.uri())
            .chat(request(vec![], None))
            .await
            .expect_err("402 is permanent");
        let SlagError::ProviderApi(api) = &err else {
            panic!("expected typed error, got: {err}");
        };
        assert_eq!(api.status, Some(402));
        assert_eq!(api.category, ProviderErrorCategory::Billing);
        assert!(!api.retryable);
        assert!(api.excerpt.contains("insufficient credits"), "{}", api.excerpt);
        let msg = err.to_string();
        assert!(msg.contains("credit balance low"), "{msg}");
        assert!(msg.contains("(402)"), "{msg}");
    }

    #[tokio::test]
    async fn malformed_200_is_typed_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let err = pinned(&server.uri())
            .chat(request(vec![], None))
            .await
            .expect_err("garbage 200");
        let SlagError::ProviderApi(api) = &err else {
            panic!("expected typed error, got: {err}");
        };
        assert_eq!(api.category, ProviderErrorCategory::BadResponse);
        assert!(!err.retryable());
        assert!(err.to_string().contains("malformed response"), "{err}");
    }
}
