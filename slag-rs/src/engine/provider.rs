//! provider: chat-completions client for the native forging engine.
//!
//! OpenAI-compatible wire format. Retries 429/5xx with exponential backoff,
//! fails fast on other 4xx. Provider quirks stay quarantined here; the rest
//! of the engine sees only `NormalizedResponse`.

use std::future::Future;
use std::pin::Pin;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    CancelFlag, ChatMessage, ChatRequest, EngineEvent, EventTx, FinishReason, NormalizedResponse,
    Provider, RetryPolicy, ToolCall, Usage,
};
use crate::config::ProviderPreset;
use crate::error::{ProviderApiError, ProviderErrorCategory, SlagError};

const DEFAULT_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_MAX_ATTEMPTS: usize = 8;
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CAP_MS: u64 = 32_000;
const RETRY_AFTER_CAP_SECS: u64 = 60;
const BODY_EXCERPT_LEN: usize = 300;
const RATE_LIMIT_BACKOFF_CAP_MS: u64 = 30_000;
const RATE_LIMIT_IDLE: Duration = Duration::from_secs(60);
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

#[cfg(not(test))]
static MODEL_INDEX_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, std::sync::Arc<ModelsIndex>>>> = OnceLock::new();

#[derive(Default)]
struct RateLimitState {
    failures: usize,
    deadline: Option<tokio::time::Instant>,
    // Keep retries serial for one quiet minute after the cooldown.
    gate_until: Option<tokio::time::Instant>,
}

#[derive(Default)]
struct RateLimiter {
    state: std::sync::Mutex<RateLimitState>,
    gate: tokio::sync::Mutex<()>,
}

// Keep throttle state across short-lived smith clients.
static RATE_LIMITERS: OnceLock<std::sync::Mutex<HashMap<String, std::sync::Arc<RateLimiter>>>> = OnceLock::new();

fn shared_rate_limiter(provider: ProviderPreset, base_url: &str) -> std::sync::Arc<RateLimiter> {
    let host = reqwest::Url::parse(base_url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| base_url.to_string());
    let key = format!("{}::{host}", provider.name());
    let mut limiters = RATE_LIMITERS.get_or_init(Default::default).lock().unwrap();
    limiters.retain(|_, limiter| {
        std::sync::Arc::strong_count(limiter) > 1
            || limiter.active()
    });
    limiters.entry(key).or_default().clone()
}

impl RateLimiter {
    fn delay(&self, hint: Option<Duration>) -> Duration {
        let mut state = self.state.lock().unwrap();
        if state.gate_until.is_some_and(|until| until <= tokio::time::Instant::now()) {
            state.failures = 0;
        }
        state.failures = state.failures.saturating_add(1);
        let delay = hint.unwrap_or_else(|| rate_limit_backoff(state.failures, next_jitter()));
        let now = tokio::time::Instant::now();
        let deadline = state.deadline.map_or(now + delay, |previous| previous.max(now + delay));
        state.deadline = Some(deadline);
        state.gate_until = Some(deadline + RATE_LIMIT_IDLE);
        deadline.saturating_duration_since(now)
    }

    fn defer(&self, delay: Duration) {
        let mut state = self.state.lock().unwrap();
        let target = tokio::time::Instant::now() + delay;
        let deadline = state.deadline.map_or(target, |previous| previous.max(target));
        state.deadline = Some(deadline);
        state.gate_until = Some(deadline + RATE_LIMIT_IDLE);
    }

    fn remaining(&self) -> Duration {
        self.state.lock().unwrap().deadline
            .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
            .unwrap_or_default()
    }

    fn active(&self) -> bool {
        self.state.lock().unwrap().gate_until.is_some_and(|until| until > tokio::time::Instant::now())
    }

    fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.failures = 0;
        state.deadline = None;
    }
}

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
    let mut builder = reqwest::Client::builder().timeout(timeout).redirect(reqwest::redirect::Policy::none());
    if drop_idle_pool {
        builder = builder.pool_max_idle_per_host(0);
    }
    builder.build().expect("reqwest client build")
}

/// OpenRouter chat-completions client.
pub struct OpenRouter {
    provider: ProviderPreset,
    quirks: std::sync::atomic::AtomicU8,
    routes: std::sync::Mutex<Vec<std::sync::Arc<OpenRouter>>>,
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
    rate_limiter: std::sync::Arc<RateLimiter>,
}

impl OpenRouter {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_provider(ProviderPreset::OpenRouter, api_key, super::OPENROUTER_BASE)
    }

    /// Base URL override enables wiremock tests and proxies.
    pub fn with_base_url(api_key: impl Into<String>, url: impl Into<String>) -> Self {
        let provider = crate::config::active_provider().unwrap_or(ProviderPreset::OpenRouter);
        Self::with_provider(provider, api_key, url)
    }

    pub fn with_provider(
        provider: ProviderPreset,
        api_key: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        let timeout = timeout_from_env();
        let base_url = url.into().trim_end_matches('/').to_string();
        let rate_limiter = shared_rate_limiter(provider, &base_url);
        Self {
            provider,
            quirks: std::sync::atomic::AtomicU8::new(0),
            routes: std::sync::Mutex::new(Vec::new()),
            api_key: api_key.into(),
            base_url,
            http: std::sync::Mutex::new(build_client(timeout, false)),
            timeout,
            max_attempts: if provider == ProviderPreset::OpenRouter {
                max_attempts_from_env()
            } else {
                max_attempts_from_env().min(3)
            },
            fallback_model: crate::config::fallback_model(),
            unattended: provider == ProviderPreset::OpenRouter && crate::config::unattended_retry(),
            events: std::sync::Mutex::new(None),
            cancel: std::sync::Mutex::new(None),
            models: tokio::sync::Mutex::new(None),
            rate_limiter,
        }
    }

    fn routed_client(&self, provider: ProviderPreset, key: String, base_url: String) -> std::sync::Arc<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let mut routes = self.routes.lock().unwrap();
        if let Some(routed) = routes.iter().find(|r| r.provider == provider && r.api_key == key && r.base_url == base_url) {
            return routed.clone();
        }
        let routed = std::sync::Arc::new(Self::with_provider(provider, key, base_url));
        *routed.events.lock().unwrap() = self.events.lock().unwrap().clone();
        *routed.cancel.lock().unwrap() = self.cancel.lock().unwrap().clone();
        routes.push(routed.clone());
        routed
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
        let (provider, key, base_url, model) = crate::config::resolve_connection(
            model, self.provider, &self.api_key, &self.base_url,
        ).ok()?;
        if provider != ProviderPreset::OpenRouter {
            return None;
        }
        if provider == self.provider && base_url == self.base_url {
            self.models_index().await.window(&model)
        } else {
            self.routed_client(provider, key, base_url).models_index().await.window(&model)
        }
    }

    /// The `/models` index, fetched at most once per client. Both the
    /// window cache and the price table come out of this single request.
    /// A failed fetch still caches (empty windows, disk-cached prices), so
    /// an offline run does not re-fetch on every turn.
    async fn models_index(&self) -> std::sync::Arc<ModelsIndex> {
        if !self.provider.has_models_endpoint() {
            return std::sync::Arc::new(ModelsIndex::default());
        }
        let mut slot = self.models.lock().await;
        if let Some(index) = slot.as_ref() {
            return index.clone();
        }
        #[cfg(not(test))]
        let (mut process_cache, cache_key) = {
            let cache = MODEL_INDEX_CACHE.get_or_init(Default::default);
            let cache_key = format!("{:?}:{}", self.provider, self.base_url);
            let process_cache = cache.lock().await;
            if let Some(index) = process_cache.get(&cache_key) {
                *slot = Some(index.clone());
                return index.clone();
            }
            (process_cache, cache_key)
        };
        let body = fetch_models_body_for(&self.api_key, &self.base_url).await;
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
        #[cfg(not(test))]
        process_cache.insert(cache_key, index.clone());
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
    async fn attribute(&self, resp: &mut NormalizedResponse, req: &ChatRequest, cached_tokens: u64) {
        let ran = resp.model.clone().unwrap_or_else(|| req.model.clone());
        resp.usage.role = Some(req.role);
        resp.usage.model = Some(if self.provider == ProviderPreset::OpenRouter {
            ran.clone()
        } else {
            format!("{}::{ran}", self.provider.name())
        });
        let has_token_split = resp.usage.prompt_tokens > 0 || resp.usage.completion_tokens > 0;
        let override_price = crate::engine::pricing::override_price(self.provider, &ran)
            .or_else(|| crate::engine::pricing::override_price(self.provider, &req.model));
        if let Some(price) = override_price.filter(|_| has_token_split) {
            resp.usage.cost = Some(price.cost_of(&resp.usage, cached_tokens));
            resp.usage.estimated = true;
        } else if resp.usage.cost.is_none() && has_token_split {
            let cost = if self.provider == ProviderPreset::OpenRouter {
                self.models_index().await.prices.estimate(&ran, &resp.usage)
            } else {
                crate::engine::pricing::builtin_price(self.provider, &ran)
                    .or_else(|| crate::engine::pricing::builtin_price(self.provider, &req.model))
                    .map(|price| price.cost_of(&resp.usage, cached_tokens))
            };
            if let Some(cost) = cost {
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
        });
        if self.provider == ProviderPreset::OpenRouter {
            body["usage"] = json!({ "include": true });
        } else if let Some(messages) = body["messages"].as_array_mut() {
            for (message, original) in messages.iter_mut().zip(&req.messages) {
                message.as_object_mut().unwrap().remove("reasoning_details");
                replay_provider_metadata(message, original.reasoning_details.as_ref(), self.provider);
            }
        }
        // OpenRouter's native fallback routing: when the primary sheds
        // load (429/529-class), the router retries the same request on
        // the fallback inside one round trip — no client-side retry turn
        // needed. The response's `model` field then names the fallback,
        // which the agent already surfaces as a `ModelRouted` event.
        if self.provider == ProviderPreset::OpenRouter {
            if let Some(fb) = self.fallback_model.as_deref().filter(|fb| *fb != req.model) {
                body["models"] = json!([req.model, fb]);
            }
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
        if let Some(effort) = req.effort {
            if self.provider == ProviderPreset::OpenRouter {
                body["reasoning"] = json!({ "effort": effort.as_str() });
            } else if let Some(effort) = reasoning_effort(self.provider, &req.model, effort) {
                body["reasoning_effort"] = json!(effort);
            }
        }
        if let Some(max) = req.max_tokens {
            let field = if self.provider == ProviderPreset::OpenAI && openai_reasoning_model(&req.model) {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[field] = json!(max);
        }
        apply_quirks(&mut body, self.quirks.load(std::sync::atomic::Ordering::Relaxed));
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

    fn rate_limit_status(&self, req: &ChatRequest, delay: Duration) {
        let message = format!("{}, role {}, model {}: rate limit (429); retry in {:.1}s.",
            self.provider.name(), req.role, req.model, delay.as_secs_f64());
        if let Some(tx) = self.events.lock().unwrap().as_ref() {
            let _ = tx.send(EngineEvent::Warning { message });
        } else if !crate::tui::is_quiet() {
            eprintln!("    {message}");
        }
    }

    async fn rate_limit_permit(&self, req: &ChatRequest) -> Result<Option<tokio::sync::MutexGuard<'_, ()>>, SlagError> {
        if !self.rate_limiter.active() {
            return Ok(None);
        }
        let gate = self.rate_limiter.gate.lock();
        tokio::pin!(gate);
        let guard = loop {
            tokio::select! {
                guard = &mut gate => break guard,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if self.is_cancelled() {
                        return Err(SlagError::Cancelled);
                    }
                    if let Some(error) = crate::error::provider_run_failure() {
                        return Err(error);
                    }
                }
            }
        };
        if !self.rate_limiter.active() {
            return Ok(None);
        }
        let delay = self.rate_limiter.remaining();
        if !delay.is_zero() {
            self.rate_limit_status(req, delay);
        }
        loop {
            if self.is_cancelled() {
                return Err(SlagError::Cancelled);
            }
            if let Some(error) = crate::error::provider_run_failure() {
                return Err(error);
            }
            let remaining = self.rate_limiter.remaining();
            if remaining.is_zero() {
                return Ok(Some(guard));
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
        }
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
    #[allow(clippy::too_many_arguments)]
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

    async fn chat_impl(&self, mut req: ChatRequest) -> Result<NormalizedResponse, SlagError> {
        let requested = req.model.clone();
        let (provider, key, base_url, model) = crate::config::resolve_connection(
            &req.model, self.provider, &self.api_key, &self.base_url,
        )?;
        req.model = model.clone();
        let mut response = if provider != self.provider || key != self.api_key || base_url != self.base_url {
            self.routed_client(provider, key, base_url).chat_direct(req).await?
        } else {
            self.chat_direct(req).await?
        };
        if requested != model && response.model.as_deref() == Some(model.as_str()) {
            response.model = Some(requested);
        }
        Ok(response)
    }

    fn error_context(&self, req: &ChatRequest, category: ProviderErrorCategory, detail: &str) -> String {
        let provider = self.provider.name();
        let remedy = provider_remedy(self.provider, category, &self.base_url);
        let separator = if detail.ends_with(['.', '!', '?']) { "" } else { "." };
        let message = format!("{provider}, role {}, model {}. {detail}{separator} {remedy}", req.role, req.model);
        if self.api_key.is_empty() { message } else { message.replace(&self.api_key, "[redacted]") }
    }

    async fn chat_direct(&self, req: ChatRequest) -> Result<NormalizedResponse, SlagError> {
        crate::config::validate_provider_url(self.provider, &self.base_url)?;
        #[cfg(not(test))]
        if self.provider.has_models_endpoint() {
            let models = self.models_index().await;
            if !models.ids.is_empty() && !models.contains(&req.model) {
                return Err(SlagError::ProviderApi(ProviderApiError {
                    status: Some(404),
                    category: ProviderErrorCategory::InvalidRequest,
                    retryable: false,
                    excerpt: self.error_context(&req, ProviderErrorCategory::InvalidRequest, &format!("model {} not found", req.model)),
                }));
            }
        }
        let mut body = self.build_body(&req);
        let mut shape_retries = 0usize;
        let url = format!("{}/chat/completions", self.base_url);
        // Definite-assignment checked: every `break` below writes it first.
        let mut last_err;
        let mut last_category;
        let mut attempts_made = 0usize;
        let mut budget_used = 0usize;
        let mut unattended_waited = Duration::ZERO;
        // Set by the previous failure's plan: how to wait before this
        // attempt and whether it consumes the bounded budget.
        let mut next: Option<RetryPlan> = None;

        loop {
            if let Some(error) = crate::error::provider_run_failure() {
                return Err(error);
            }
            let free = match next.take() {
                Some(plan) => {
                    if (plan.status != 429 || plan.heartbeats) && !self.wait_out(attempts_made, &plan).await {
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

            let rate_limit_permit = self.rate_limit_permit(&req).await?;
            let client = self.http.lock().unwrap().clone();
            let mut request = client.post(&url);
            if !self.api_key.is_empty() {
                request = request.bearer_auth(&self.api_key);
            }
            if self.provider == ProviderPreset::OpenRouter {
                request = request
                    .header("HTTP-Referer", "https://slag.dev")
                    .header("X-Title", "slag");
            }
            let sent = request.json(&body).send().await;

            let resp = match sent {
                Ok(resp) => resp,
                // Connect and timeout failures are transient by definition;
                // the remaining send errors rarely heal but a bounded retry
                // costs little.
                Err(e) => {
                    if is_stale_connection(&e) {
                        self.rebuild_client();
                    }
                    last_category = ProviderErrorCategory::Network;
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
                            excerpt: self.error_context(&req, ProviderErrorCategory::BadResponse, &format!("malformed response: {e}")),
                        }))
                    }
                };
                let cached_tokens = api.usage.as_ref().map(ApiUsage::cached_tokens).unwrap_or(0);
                match normalize_for_provider(api, self.provider) {
                    // Empty 200s (no choices, no message, no finish_reason)
                    // are upstream hiccups; burn a retry instead of handing
                    // the agent an empty turn.
                    Err(SlagError::ProviderTransient(why)) => {
                        last_category = ProviderErrorCategory::BadResponse;
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
                        if rate_limit_permit.is_some() {
                            self.rate_limiter.reset();
                        }
                        self.attribute(&mut resp, &req, cached_tokens).await;
                        return Ok(resp);
                    }
                    other => return other,
                }
            }

            let rate_limit_header = resp.headers().keys().any(|name|
                name == reqwest::header::RETRY_AFTER || name.as_str().starts_with("x-ratelimit-"));
            let retry_after_header = parse_retry_after(resp.headers());
            let reset_wait = parse_ratelimit_reset(resp.headers(), now_epoch_ms());
            let code = status.as_u16();
            let mut text = resp.text().await.unwrap_or_default();
            if !self.api_key.is_empty() {
                text = text.replace(&self.api_key, "[redacted]");
            }
            let excerpt = excerpt(&text);
            if matches!(code, 400 | 422) && shape_retries < 3
                && budget_used < req.retry.attempts.unwrap_or(self.max_attempts) {
                if let Some(quirk) = rejected_tool_shape(&body, &text) {
                    self.quirks.fetch_or(quirk, std::sync::atomic::Ordering::Relaxed);
                    apply_quirks(&mut body, quirk);
                    shape_retries += 1;
                    continue;
                }
            }
            if code == 400 && !req.tools.is_empty() && is_tool_validation_error(&text) {
                let names = req.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>().join(", ");
                let mut feedback = self.error_context(&req, ProviderErrorCategory::InvalidRequest, &tool_validation_message(&text));
                feedback.push_str(&format!(" Use only these declared tool names: {names}."));
                if req.tools.iter().any(|tool| tool.name == "finish") {
                    feedback.push_str(" To complete the task, call finish with cmd and summary.");
                }
                return Err(SlagError::ToolValidation(feedback));
            }
            let category = classify_response(code, &text, rate_limit_header);
            let deterministic = matches!(category, ProviderErrorCategory::Auth | ProviderErrorCategory::Billing | ProviderErrorCategory::InvalidRequest);
            if !deterministic && retry_hint.unwrap_or_else(|| transient_status(status)) {
                // Any transient status may carry Retry-After — 429 rate
                // limits and 502/503 gateway drains alike — so honor it
                // whenever the server sent one, not only on 429.
                last_category = category;
                last_err = format!("{status}: {excerpt}");
                let retry_after = retry_after_header.or_else(|| if code == 429 { parse_retry_after_body(&text) } else { None });
                let retry_after = if code == 429 {
                    let hint = if self.unattended && req.retry.persistent {
                        reset_wait.or(retry_after)
                    } else {
                        retry_after.or(reset_wait.map(|delay| delay.min(Duration::from_secs(RETRY_AFTER_CAP_SECS))))
                    };
                    Some(self.rate_limiter.delay(hint))
                } else {
                    retry_after
                };
                match self.plan_retry(
                    Some(code),
                    retry_after,
                    if code == 429 { None } else { reset_wait },
                    attempts_made,
                    budget_used,
                    unattended_waited,
                    req.retry,
                ) {
                    Some(plan) => {
                        if code == 429 {
                            self.rate_limiter.defer(plan.delay);
                        }
                        if category == ProviderErrorCategory::RateLimit && plan.heartbeats {
                            self.rate_limit_status(&req, plan.delay);
                        }
                        next = Some(plan);
                        continue;
                    }
                    None => break,
                }
            }
            return Err(SlagError::ProviderApi(ProviderApiError {
                status: Some(code),
                category,
                retryable: false,
                excerpt: self.error_context(&req, category, &excerpt),
            }));
        }

        Err(SlagError::ProviderTransient(self.error_context(&req, last_category,
            &format!("gave up after {attempts_made} attempts: {last_err}"))))
    }
}

fn openai_reasoning_model(model: &str) -> bool {
    model == "o1" || model.starts_with("o1-")
        || model == "o3" || model.starts_with("o3-")
        || model == "o4" || model.starts_with("o4-")
        || model.starts_with("gpt-5")
}

fn reasoning_effort(provider: ProviderPreset, model: &str, effort: super::Effort) -> Option<&'static str> {
    match provider {
        ProviderPreset::OpenAI if openai_reasoning_model(model) && !model.starts_with("o1-preview")
            && !model.starts_with("o1-mini") && !model.contains("-chat-") => Some(effort.as_str()),
        ProviderPreset::Gemini if model.starts_with("gemini-2.5") => Some(effort.as_str()),
        ProviderPreset::Gemini if model.starts_with("gemini-3") => Some(match effort {
            super::Effort::Medium if !model.contains("flash") => "high",
            _ => effort.as_str(),
        }),
        ProviderPreset::Groq | ProviderPreset::Cerebras if model.contains("gpt-oss") => Some(effort.as_str()),
        // Measured 2026-10-17: qwen-3.8-27b answers a short review in 1-2 s
        // with effort low and 15-27 s without it.
        ProviderPreset::Cerebras if model.starts_with("qwen-3") => Some(effort.as_str()),
        ProviderPreset::Xai if model.starts_with("grok-3-mini") => Some(match effort {
            super::Effort::Low => "low",
            _ => "high",
        }),
        _ => None,
    }
}

const OMIT_TOOL_CHOICE: u8 = 1;
const OMIT_PARALLEL_TOOLS: u8 = 2;
const OMIT_EMPTY_TOOL_CONTENT: u8 = 4;

fn apply_quirks(body: &mut Value, quirks: u8) {
    let object = body.as_object_mut().unwrap();
    if quirks & OMIT_TOOL_CHOICE != 0 {
        object.remove("tool_choice");
    }
    if quirks & OMIT_PARALLEL_TOOLS != 0 {
        object.remove("parallel_tool_calls");
    }
    if quirks & OMIT_EMPTY_TOOL_CONTENT != 0 {
        if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if message["role"] == "assistant"
                    && message.get("tool_calls").and_then(Value::as_array).is_some_and(|calls| !calls.is_empty())
                    && message.get("content").is_some_and(|content| content.is_null() || content.as_str() == Some(""))
                {
                    message.as_object_mut().unwrap().remove("content");
                }
            }
        }
    }
}

fn rejected_tool_shape(body: &Value, error: &str) -> Option<u8> {
    let error = error.to_lowercase();
    if !["unsupported", "not supported", "unknown", "unrecognized", "not permitted", "not allowed",
        "invalid", "must", "cannot", "empty", "null", "required", "extra inputs"].iter().any(|s| error.contains(s)) {
        return None;
    }
    if error.contains("tool_choice") && body.get("tool_choice").is_some() {
        return Some(OMIT_TOOL_CHOICE);
    }
    if error.contains("parallel_tool_calls") && body.get("parallel_tool_calls").is_some() {
        return Some(OMIT_PARALLEL_TOOLS);
    }
    if error.contains("content") && (error.contains("empty") || error.contains("null") || error.contains("tool_call")) {
        let mut adjusted = body.clone();
        apply_quirks(&mut adjusted, OMIT_EMPTY_TOOL_CONTENT);
        if adjusted != *body {
            return Some(OMIT_EMPTY_TOOL_CONTENT);
        }
    }
    None
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

fn provider_remedy(provider: ProviderPreset, category: ProviderErrorCategory, base_url: &str) -> String {
    match provider {
        ProviderPreset::Ollama if category == ProviderErrorCategory::Network => return "Start the server with `ollama serve`.".into(),
        ProviderPreset::LmStudio if category == ProviderErrorCategory::Network => return "Start LM Studio's local server.".into(),
        _ => {}
    }
    match category {
        ProviderErrorCategory::Auth => match provider.key_env() {
            Some(env) => format!("Set {env} or run `slag key --provider {}`. The environment key overrides the saved key.", provider.name()),
            None => "Check the local server's authentication settings.".into(),
        },
        ProviderErrorCategory::Billing => {
            let url = match provider {
                ProviderPreset::OpenRouter => "https://openrouter.ai/credits",
                ProviderPreset::OpenAI => "https://platform.openai.com/settings/organization/billing/overview",
                ProviderPreset::Anthropic => "https://console.anthropic.com/settings/billing",
                ProviderPreset::Gemini => "https://aistudio.google.com/usage",
                ProviderPreset::Groq => "https://console.groq.com/settings/billing",
                ProviderPreset::Cerebras => "https://cloud.cerebras.ai/platform/billing",
                ProviderPreset::Xai => "https://console.x.ai/team/default/billing",
                ProviderPreset::Mistral => "https://admin.mistral.ai/organization/billing",
                ProviderPreset::DeepSeek => "https://platform.deepseek.com/top_up",
                ProviderPreset::Together => "https://api.together.ai/settings/billing",
                ProviderPreset::Fireworks => "https://app.fireworks.ai/account/billing",
                _ => return "Check the server's credits and spend limit.".into(),
            };
            format!("Check credits and spend limits at {url}.")
        }
        ProviderErrorCategory::InvalidRequest => {
            let auth = provider.key_env().map(|env| format!(" -H \"Authorization: Bearer ${env}\"" )).unwrap_or_default();
            format!("Check the model id and request. List models with `curl{auth} {}/models`.", base_url.trim_end_matches('/'))
        }
        ProviderErrorCategory::RateLimit => "Wait for the rate limit to reset or reduce parallel calls.".into(),
        ProviderErrorCategory::Network => "Check the API base URL and network connection.".into(),
        ProviderErrorCategory::BadResponse => "Check the endpoint's chat-completions response format.".into(),
        _ => "Retry after the provider restores service.".into(),
    }
}

fn classify_response(status: u16, body: &str, rate_limit_header: bool) -> ProviderErrorCategory {
    let body = body.to_ascii_lowercase();
    if status == 429 && (rate_limit_header || body.contains("rate limit") || body.contains("try again in")) {
        return ProviderErrorCategory::RateLimit;
    }
    if status == 402 || (matches!(status, 403 | 429) && [
        "insufficient_quota", "insufficient quota", "insufficient credit", "out of credit",
        "credits exhausted", "credit balance", "spend limit", "spending limit", "billing limit",
        "payment required", "exceeded your current quota", "monthly limit", "budget exceeded",
        "no credits", "credit limit", "run out of credits", "purchase credits", "exhausted its credits",
        "exhausted your credits", "billing_hard_limit_reached", "daily spend limit",
    ].iter().any(|text| body.contains(text))) {
        return ProviderErrorCategory::Billing;
    }
    if matches!(status, 400 | 403 | 404 | 422) && [
        "invalid api key", "invalid_api_key", "incorrect api key", "wrong api key",
        "missing api key", "authentication", "unauthorized",
    ].iter().any(|text| body.contains(text)) {
        return ProviderErrorCategory::Auth;
    }
    if matches!(status, 400 | 403 | 404 | 422) && [
        "model_not_found", "model not found", "invalid model", "model does not exist",
        "model_not_available", "unknown model",
    ].iter().any(|text| body.contains(text)) {
        return ProviderErrorCategory::InvalidRequest;
    }
    classify_status(status)
}

/// Provider error category from an HTTP status, decided exactly once.
fn classify_status(status: u16) -> ProviderErrorCategory {
    match status {
        401 | 403 => ProviderErrorCategory::Auth,
        402 => ProviderErrorCategory::Billing,
        408 => ProviderErrorCategory::Network,
        409 => ProviderErrorCategory::Server,
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

/// Honor Retry-After seconds or an HTTP date, with a 60-second cap.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    parse_retry_after_at(headers, std::time::SystemTime::now())
}

fn parse_retry_after_body(body: &str) -> Option<Duration> {
    let pattern = regex::Regex::new(r"(?i)try again in\s+([0-9]+(?:\.[0-9]+)?)\s*(?:s\b|seconds?\b)").unwrap();
    let seconds: f64 = pattern.captures(body)?.get(1)?.as_str().parse().ok()?;
    Some(Duration::from_secs_f64(seconds.min(RETRY_AFTER_CAP_SECS as f64)))
}

fn parse_retry_after_at(headers: &reqwest::header::HeaderMap, now: std::time::SystemTime) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    let delay = if let Ok(secs) = raw.parse::<u64>() {
        Duration::from_secs(secs)
    } else {
        let date = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
        let target: std::time::SystemTime = date.into();
        target.duration_since(now).unwrap_or_default()
    };
    Some(delay.min(Duration::from_secs(RETRY_AFTER_CAP_SECS)))
}

/// Process-wide LCG state for retry jitter — enough randomness to spread
/// parallel anvils without pulling in a rand dependency.
static JITTER_STATE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0x2545_F491_4F6C_DD1D);

fn next_jitter() -> u64 {
    use std::sync::atomic::Ordering;
    let step = |s: u64| {
        s.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
    };
    let prev = JITTER_STATE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |s| Some(step(s)))
        .expect("closure always returns Some");
    // High bits of the advanced state; low LCG bits cycle too predictably.
    step(prev) >> 1
}

/// Draw full jitter from the exponential window, capped at 30 seconds.
fn rate_limit_backoff(retry: usize, draw: u64) -> Duration {
    let ceiling = (1_000u64 << retry.saturating_sub(1).min(5)).min(RATE_LIMIT_BACKOFF_CAP_MS);
    Duration::from_millis(draw % (ceiling + 1))
}

/// 0..=25, a fresh percentage per call.
fn next_jitter_percent() -> u64 {
    next_jitter() % 26
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
        Box::pin(async move {
            if let Some(error) = crate::error::provider_run_failure() {
                return Err(error);
            }
            let result = self.chat_impl(req).await;
            if let Err(error) = &result {
                crate::error::record_provider_failure(error);
            }
            crate::error::provider_run_failure().map_or(result, Err)
        })
    }

    /// Wire the heartbeat sink: `ApiRetry` events flow here during
    /// unattended waits.
    fn set_event_sink(&self, tx: EventTx) {
        *self.events.lock().unwrap() = Some(tx.clone());
        for route in self.routes.lock().unwrap().iter() {
            route.set_event_sink(tx.clone());
        }
    }

    /// Wire the cancel flag: retry waits abort when it goes up.
    fn set_cancel_flag(&self, f: CancelFlag) {
        *self.cancel.lock().unwrap() = Some(f.clone());
        for route in self.routes.lock().unwrap().iter() {
            route.set_cancel_flag(f.clone());
        }
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
    let provider = crate::config::active_provider().ok()?;
    if provider != ProviderPreset::OpenRouter {
        eprintln!("{} has no balance endpoint; session spend only.", provider.name());
        return None;
    }
    if api_key.is_empty() {
        return None;
    }
    crate::config::validate_provider_url(ProviderPreset::OpenRouter, base_url).ok()?;
    let url = format!("{}/credits", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
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
    if let Err(error) = crate::config::validate_provider_url(ProviderPreset::OpenRouter, base_url) {
        return KeyCheck::Rejected(error.to_string());
    }
    let url = format!("{}/key", base_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
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
        .redirect(reqwest::redirect::Policy::none())
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
    fetch_models_body_for("", base_url).await
}

async fn fetch_models_body_for(api_key: &str, base_url: &str) -> Option<String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let mut request = client.get(&url);
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let resp = request
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
    ids: HashSet<String>,
    windows: std::collections::HashMap<String, u64>,
    prices: crate::engine::pricing::PricingTable,
}

impl ModelsIndex {
    fn parse(body: Option<&str>) -> Self {
        let Some(body) = body else {
            return Self::default();
        };
        let models = serde_json::from_str::<ModelsResponse>(body).ok();
        let ids = models
            .as_ref()
            .map(|m| m.data.iter().map(|e| e.id.clone()).collect())
            .unwrap_or_default();
        let windows = serde_json::from_str::<ModelsResponse>(body)
            .map(|m| {
                m.data
                    .into_iter()
                    .filter_map(|e| e.context_length.map(|c| (e.id, c)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            ids,
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

    fn contains(&self, model: &str) -> bool {
        self.ids.contains(model) || self.ids.contains(model.split(':').next().unwrap_or(model))
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
            if let Some(details) = m.reasoning_details.as_ref().filter(|details| details.get("_slag_provider").is_none()) {
                obj["reasoning_details"] = details.clone();
            }
            obj
        })
        .collect()
}

fn replay_provider_metadata(message: &mut Value, metadata: Option<&Value>, provider: ProviderPreset) {
    let Some(metadata) = metadata.filter(|m| m["_slag_provider"] == provider.name()) else {
        return;
    };
    if provider == ProviderPreset::DeepSeek {
        if let Some(reasoning) = metadata.get("reasoning_content").and_then(Value::as_str) {
            message["reasoning_content"] = json!(reasoning);
        }
    }
    if provider == ProviderPreset::Gemini {
        if let Some(extra) = metadata.get("message_extra_content").filter(|extra| !extra.is_null()) {
            message["extra_content"] = extra.clone();
        }
        if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for call in calls {
                if let Some(extra) = call["id"].as_str()
                    .and_then(|id| metadata["tool_extra_content"].get(id)) {
                    call["extra_content"] = extra.clone();
                }
            }
        }
    }
}

/// Distinguishes synthesized fallback tool-call ids across responses so a
/// multi-turn history never replays duplicate ids to strict backends.
static FALLBACK_ID_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn normalize(api: ApiResponse) -> Result<NormalizedResponse, SlagError> {
    normalize_for_provider(api, ProviderPreset::OpenRouter)
}

fn normalize_for_provider(api: ApiResponse, provider: ProviderPreset) -> Result<NormalizedResponse, SlagError> {
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

    let tool_metadata: Vec<Option<Value>> = msg.tool_calls.as_deref().unwrap_or_default()
        .iter().map(|call| call.extra_content.clone()).collect();
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

    let reasoning_details = match provider {
        ProviderPreset::OpenRouter => msg.reasoning_details.clone(),
        ProviderPreset::DeepSeek => msg.reasoning_content.as_ref().map(|reasoning| json!({
            "_slag_provider": provider.name(), "reasoning_content": reasoning,
        })),
        ProviderPreset::Gemini => {
            let extras: serde_json::Map<String, Value> = tool_calls.iter().zip(tool_metadata)
                .filter_map(|(call, extra)| extra.map(|extra| (call.id.clone(), extra))).collect();
            (!extras.is_empty() || msg.extra_content.is_some()).then(|| json!({
                "_slag_provider": provider.name(), "tool_extra_content": extras,
                "message_extra_content": msg.extra_content,
            }))
        }
        _ => None,
    };
    let reasoning = msg
        .reasoning
        .filter(|s| !s.is_empty())
        .or(msg.reasoning_content.filter(|s| !s.is_empty()))
        .or_else(|| reasoning_from_details(msg.reasoning_details.as_ref()));

    let mut usage = api.usage.map(|usage| usage.usage).unwrap_or_default();
    if usage.total_tokens == 0 {
        usage.total_tokens = usage.prompt_tokens.saturating_add(usage.completion_tokens);
    }
    Ok(NormalizedResponse {
        model: api.model.filter(|m| !m.is_empty()),
        content: msg.content.unwrap_or_default(),
        tool_calls,
        finish_reason,
        reasoning,
        reasoning_details,
        usage,
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

fn is_tool_validation_error(text: &str) -> bool {
    let root = serde_json::from_str::<Value>(text).ok();
    let error = root.as_ref().map(|root| root.get("error").unwrap_or(root));
    let code = error.and_then(|error| error.get("code")).and_then(Value::as_str).unwrap_or("");
    let message = error.and_then(|error| error.get("message")).and_then(Value::as_str)
        .or_else(|| error.and_then(Value::as_str)).unwrap_or(text).to_ascii_lowercase();
    code == "tool_use_failed" || message.contains("tool call validation failed")
        || message.contains("failed to call a function")
}

fn tool_validation_message(text: &str) -> String {
    let mut message = excerpt(text);
    if let Ok(root) = serde_json::from_str::<Value>(text) {
        let error = root.get("error").unwrap_or(&root);
        if let Some(generation) = error.get("failed_generation") {
            let generation = generation.as_str().map(str::to_string).unwrap_or_else(|| generation.to_string());
            message.push_str(". Rejected tool call: ");
            message.push_str(&excerpt(&generation));
        }
    }
    message
}

fn excerpt(text: &str) -> String {
    let parsed = serde_json::from_str::<Value>(text).ok();
    let message = parsed.as_ref().and_then(|root| {
        let error = root.get("error").unwrap_or(root);
        error.get("message").and_then(Value::as_str).or_else(|| error.as_str())
    });
    let trimmed = message.unwrap_or(text).trim();
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
    usage: Option<ApiUsage>,
    /// Which model actually answered. With `openrouter/auto` this is the
    /// only place the routed id appears.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct ApiUsage {
    #[serde(flatten)]
    usage: Usage,
    #[serde(default)]
    prompt_tokens_details: Option<CachedUsage>,
    #[serde(default)]
    prompt_cache_hit_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

impl ApiUsage {
    fn cached_tokens(&self) -> u64 {
        self.prompt_tokens_details.as_ref().map(|details| details.cached_tokens).unwrap_or(0)
            .max(self.prompt_cache_hit_tokens).max(self.cache_read_input_tokens)
    }
}

#[derive(Deserialize)]
struct CachedUsage {
    #[serde(default)]
    cached_tokens: u64,
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
    reasoning_content: Option<String>,
    #[serde(default)]
    extra_content: Option<Value>,
    #[serde(default)]
    reasoning_details: Option<Value>,
}

#[derive(Deserialize)]
struct ApiToolCall {
    #[serde(default)]
    id: String,
    function: ApiFunction,
    #[serde(default)]
    extra_content: Option<Value>,
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
        OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", base_url)
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
    fn model_index_rejects_unknown_model_ids() {
        let index = ModelsIndex::parse(Some(r#"{"data":[{"id":"known-model"}]}"#));
        assert!(index.contains("known-model"));
        assert!(!index.contains("unknown-model"));
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-bad", server.uri());
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

            let err = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-bad", &server.uri())
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
    fn rate_limit_full_jitter_grows_and_caps_at_thirty_seconds() {
        for (retry, ceiling) in [(1, 1_000), (2, 2_000), (3, 4_000), (4, 8_000), (5, 16_000), (6, 30_000), (100, 30_000)] {
            assert_eq!(rate_limit_backoff(retry, ceiling), Duration::from_millis(ceiling));
            assert_eq!(rate_limit_backoff(retry, ceiling + 1), Duration::ZERO);
            let draws = (0..100).map(|_| rate_limit_backoff(retry, next_jitter())).collect::<Vec<_>>();
            assert!(draws.iter().all(|delay| *delay <= Duration::from_millis(ceiling)));
            assert!(draws.iter().collect::<HashSet<_>>().len() > 1);
        }
    }

    #[tokio::test]
    async fn rate_limit_backoff_survives_new_clients_and_keeps_hosts_separate() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limit"))
            .expect(7).mount(&server).await;
        let mut saved: Option<std::sync::Weak<RateLimiter>> = None;
        for expected in 1..=7 {
            let provider = OpenRouter::with_provider(ProviderPreset::Mistral, "test-key", server.uri())
                .with_max_attempts(1);
            if let Some(limiter) = &saved {
                assert!(std::sync::Arc::ptr_eq(&limiter.upgrade().expect("limiter survives client drop"), &provider.rate_limiter));
            }
            assert!(provider.chat(request(vec![], None)).await.is_err());
            let mut state = provider.rate_limiter.state.lock().unwrap();
            assert_eq!(state.failures, expected);
            assert!(state.deadline.unwrap().saturating_duration_since(tokio::time::Instant::now())
                <= Duration::from_millis((1_000 << (expected - 1).min(5)).min(RATE_LIMIT_BACKOFF_CAP_MS)));
            state.deadline = None;
            drop(state);
            saved = Some(std::sync::Arc::downgrade(&provider.rate_limiter));
        }
        let limiter = saved.unwrap().upgrade().unwrap();
        let path_alias = shared_rate_limiter(ProviderPreset::Mistral, &format!("{}/other-path", server.uri()));
        assert!(std::sync::Arc::ptr_eq(&limiter, &path_alias));
        let other_preset = shared_rate_limiter(ProviderPreset::OpenAI, &server.uri());
        assert!(!std::sync::Arc::ptr_eq(&limiter, &other_preset));
        let other_server = MockServer::start().await;
        let other_host = shared_rate_limiter(ProviderPreset::Mistral, &other_server.uri());
        assert!(!std::sync::Arc::ptr_eq(&limiter, &other_host));
    }

    #[tokio::test]
    async fn concurrent_clients_share_rate_limit_waits_and_retry_gate() {
        let server = MockServer::start().await;
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = calls.clone();
        Mock::given(method("POST"))
            .respond_with(move |_: &wiremock::Request| {
                observed.lock().unwrap().push(std::time::Instant::now());
                ResponseTemplate::new(429).set_body_string("rate limit; try again in 0.08s")
            })
            .expect(3).mount(&server).await;
        let first = OpenRouter::with_provider(ProviderPreset::Mistral, "test-key", server.uri())
            .with_max_attempts(1);
        assert!(first.chat(request(vec![], None)).await.is_err());
        let second = OpenRouter::with_provider(ProviderPreset::Mistral, "test-key", server.uri())
            .with_max_attempts(1);
        let third = OpenRouter::with_provider(ProviderPreset::Mistral, "test-key", server.uri())
            .with_max_attempts(1);
        let (tx, mut rx) = super::super::events::channel();
        second.set_event_sink(tx.clone());
        third.set_event_sink(tx);
        let (a, b) = tokio::join!(second.chat(request(vec![], None)), third.chat(request(vec![], None)));
        assert!(a.is_err() && b.is_err());
        let calls = calls.lock().unwrap();
        for pair in calls.windows(2) {
            assert!(pair[1].duration_since(pair[0]) >= Duration::from_millis(70));
        }
        let mut warnings = 0;
        while let Ok(event) = rx.try_recv() {
            if let EngineEvent::Warning { message } = event {
                assert!(message.contains("mistral") && message.contains("rate limit (429)"));
                warnings += 1;
            }
        }
        assert_eq!(warnings, 2, "one status line per wait");
        assert_eq!(first.rate_limiter.state.lock().unwrap().failures, 3);
    }

    #[tokio::test]
    async fn unattended_rate_limit_keeps_the_shared_gate_after_recovery() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0").set_body_string("rate limit"))
            .up_to_n_times(1).expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1).mount(&server).await;
        let provider = pinned(&server.uri()).with_max_attempts(1).with_unattended(true);
        assert!(provider.chat(request(vec![], None)).await.is_ok());
        let sibling = pinned(&server.uri()).with_unattended(true);
        assert!(std::sync::Arc::ptr_eq(&provider.rate_limiter, &sibling.rate_limiter));
        assert!(sibling.rate_limiter.active());
        assert_eq!(sibling.rate_limiter.state.lock().unwrap().failures, 0);
    }

    #[tokio::test]
    async fn in_flight_success_does_not_clear_a_new_rate_limit() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(body_partial_json(json!({"model": "slow"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()).set_delay(Duration::from_millis(100)))
            .expect(1).mount(&server).await;
        Mock::given(method("POST")).and(body_partial_json(json!({"model": "limited"})))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1")
                .set_body_string("rate limit").set_delay(Duration::from_millis(10)))
            .expect(1).mount(&server).await;
        let slow = OpenRouter::with_provider(ProviderPreset::Mistral, "test-key", server.uri()).with_max_attempts(1);
        let limited = OpenRouter::with_provider(ProviderPreset::Mistral, "test-key", server.uri()).with_max_attempts(1);
        let mut slow_req = request(vec![], None);
        slow_req.model = "slow".into();
        let mut limited_req = request(vec![], None);
        limited_req.model = "limited".into();
        let (success, failure) = tokio::join!(slow.chat(slow_req), limited.chat(limited_req));
        assert!(success.is_ok() && failure.is_err());
        assert_eq!(slow.rate_limiter.state.lock().unwrap().failures, 1);
        assert!(slow.rate_limiter.remaining() > Duration::from_millis(500));
    }

    #[tokio::test]
    async fn shared_rate_limit_wait_observes_cancellation() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60").set_body_string("rate limit"))
            .expect(1).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Mistral, "test-key", server.uri()).with_max_attempts(1);
        assert!(provider.chat(request(vec![], None)).await.is_err());
        let cancel = CancelFlag::default();
        provider.set_cancel_flag(cancel.clone());
        let started = std::time::Instant::now();
        let stop = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        let (result, ()) = tokio::join!(provider.chat(request(vec![], None)), stop);
        assert!(matches!(result, Err(SlagError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(1));
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
        headers.insert("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(parse_retry_after_at(&headers, std::time::UNIX_EPOCH), Some(Duration::from_secs(60)));
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

            let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let err = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
            .chat(request(vec![], None))
            .await
            .expect_err("400 must fail");
        assert!(!err.retryable(), "400 must be permanent: {err}");
        assert!(err.to_string().contains("400"), "message was: {err}");
    }

    #[tokio::test]
    async fn x_should_retry_true_cannot_retry_an_invalid_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(400)
                    .insert_header("x-should-retry", "true")
                    .set_body_string("invalid request"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
        let error = provider.chat(request(vec![], None)).await.unwrap_err();
        assert!(error.is_deterministic_provider());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
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

        let err = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let err = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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

        let err = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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
        let err = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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

        let err = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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

        let resp = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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

        let resp = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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
        OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri())
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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

        let provider = OpenRouter::with_provider(ProviderPreset::OpenRouter, "sk-test", server.uri());
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
                if let EngineEvent::Warning { message } = &event {
                    assert_eq!(status, 429);
                    assert!(message.contains("rate limit (429)"), "{message}");
                    continue;
                }
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

    #[tokio::test]
    async fn provider_failure_classes_include_context_and_do_not_retry_deterministic_errors() {
        for (status, body, category, remedy) in [
            (401, "Wrong API Key", ProviderErrorCategory::Auth, "XAI_API_KEY"),
            (403, "Your team has exhausted its credits. Purchase credits to continue.", ProviderErrorCategory::Billing, "console.x.ai"),
            (429, "Spend limit exceeded", ProviderErrorCategory::Billing, "console.x.ai"),
            (402, "Payment required", ProviderErrorCategory::Billing, "console.x.ai"),
            (404, "Model nope-model-404 not found", ProviderErrorCategory::InvalidRequest, "/models"),
            (400, "Invalid request parameter", ProviderErrorCategory::InvalidRequest, "Check the model id"),
            (400, "Invalid API key", ProviderErrorCategory::Auth, "slag key --provider xai"),
            (429, "Rate limit exceeded", ProviderErrorCategory::RateLimit, "rate limit"),
            (503, "Service unavailable", ProviderErrorCategory::Overloaded, "restores service"),
            (500, "Internal error", ProviderErrorCategory::Server, "restores service"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(status).insert_header("x-should-retry", "true").set_body_string(body))
                .expect(1)
                .mount(&server).await;
            let deterministic = matches!(category, ProviderErrorCategory::Auth | ProviderErrorCategory::Billing | ProviderErrorCategory::InvalidRequest);
            let provider = OpenRouter::with_provider(ProviderPreset::Xai, "secret-test-key", server.uri())
                .with_max_attempts(if deterministic { 3 } else { 1 });
            let error = provider.chat(request(vec![], None)).await.unwrap_err();
            let message = error.to_string();
            if deterministic {
                let SlagError::ProviderApi(api) = &error else { panic!("{error}") };
                assert_eq!(api.category, category);
                assert!(error.is_deterministic_provider());
                assert!(message.contains(remedy), "{message}");
            } else {
                assert!(error.retryable());
                assert!(!error.is_deterministic_provider());
            }
            assert!(message.contains("xai, role smith, model qwen/qwen3-coder"), "{message}");
            assert!(!message.contains("secret-test-key"));
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    const GROQ_TPM_BODY: &str = "Rate limit reached for model openai/gpt-oss-120b ... on tokens per minute (TPM): Limit 8000, Used 6596, Requested 3872. Please try again in 18.5s. Need more tokens? Upgrade to Dev Tier today at https://console.groq.com/settings/billing";

    #[tokio::test]
    async fn groq_tpm_with_billing_url_waits_and_retries() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string(GROQ_TPM_BODY))
            .up_to_n_times(1).expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri())
            .with_unattended(false).with_max_attempts(2);
        let (tx, mut rx) = super::super::events::channel();
        provider.set_event_sink(tx);
        let started = std::time::Instant::now();
        assert!(provider.chat(request(vec![], None)).await.is_ok());
        assert!(started.elapsed() >= Duration::from_millis(18_500));
        let EngineEvent::Warning { message } = rx.try_recv().unwrap() else { panic!("missing throttle status") };
        assert!(message.contains("groq") && message.contains("rate limit") && message.contains("18.5s"), "{message}");
        assert!(rx.try_recv().is_err(), "one status line per wait");
    }

    #[tokio::test]
    async fn rate_limit_headers_override_billing_text_on_429() {
        for header in ["retry-after", "x-ratelimit-remaining-tokens", "x-ratelimit-reset-tokens"] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(429).insert_header(header, "1").set_body_string("Check billing limits"))
                .expect(1).mount(&server).await;
            let provider = pinned(&server.uri()).with_max_attempts(1);
            let error = provider.chat(request(vec![], None)).await.unwrap_err();
            assert!(error.retryable(), "{header}: {error}");
            assert!(!error.is_deterministic_provider(), "{header}: {error}");
        }
        assert_eq!(classify_response(429, "https://console.groq.com/settings/billing", false), ProviderErrorCategory::RateLimit);
    }

    #[test]
    fn body_retry_delay_preserves_fractions_and_caps_waits() {
        assert_eq!(parse_retry_after_body(GROQ_TPM_BODY), Some(Duration::from_millis(18_500)));
        assert_eq!(parse_retry_after_body("Try again in 600s."), Some(Duration::from_secs(60)));
        assert_eq!(parse_retry_after_body("try again in 1.5 seconds"), Some(Duration::from_millis(1500)));
        for body in ["try again in NaNs", "try again in -1s", "try again in 18.5sessions", "billing"] {
            assert_eq!(parse_retry_after_body(body), None, "{body}");
        }
    }

    #[tokio::test]
    async fn quota_errors_remain_billing_without_rate_limit_signals() {
        for (preset, status, body) in [
            (ProviderPreset::Xai, 403, "Your team has exhausted its credits. Purchase credits to continue."),
            (ProviderPreset::OpenAI, 429, r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","code":"insufficient_quota"}}"#),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .expect(1).mount(&server).await;
            let provider = OpenRouter::with_provider(preset, "test-key", server.uri()).with_max_attempts(3);
            let SlagError::ProviderApi(error) = provider.chat(request(vec![], None)).await.unwrap_err() else { panic!("expected billing") };
            assert_eq!(error.category, ProviderErrorCategory::Billing);
        }
    }

    #[tokio::test]
    async fn local_connection_errors_name_the_provider_and_server_remedy() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        for (preset, remedy) in [
            (ProviderPreset::Ollama, "ollama serve"),
            (ProviderPreset::LmStudio, "Start LM Studio's local server"),
        ] {
            let provider = OpenRouter::with_provider(preset, "", &url).with_max_attempts(1);
            let message = provider.chat(request(vec![], None)).await.unwrap_err().to_string();
            assert!(message.contains(preset.name()), "{message}");
            assert!(message.contains("role smith, model qwen/qwen3-coder"), "{message}");
            assert!(message.contains(remedy), "{message}");
        }
    }

    #[test]
    fn provider_remedies_cover_each_preset_without_key_values() {
        for preset in [ProviderPreset::OpenRouter, ProviderPreset::OpenAI, ProviderPreset::Anthropic,
            ProviderPreset::Gemini, ProviderPreset::Groq, ProviderPreset::Cerebras, ProviderPreset::Xai,
            ProviderPreset::Mistral, ProviderPreset::DeepSeek, ProviderPreset::Together, ProviderPreset::Fireworks,
            ProviderPreset::Ollama, ProviderPreset::LmStudio, ProviderPreset::Custom] {
            let auth = provider_remedy(preset, ProviderErrorCategory::Auth, preset.base_url());
            if let Some(env) = preset.key_env() {
                assert!(auth.contains(env), "{auth}");
                assert!(auth.contains(&format!("slag key --provider {}", preset.name())), "{auth}");
            }
            let model = provider_remedy(preset, ProviderErrorCategory::InvalidRequest, "http://localhost:1234/v1");
            assert!(model.contains("curl"));
            assert!(model.contains("/models"));
        }
    }

    #[tokio::test]
    async fn non_router_pricing_is_local_and_cache_aware() {
        let (_guard, _dir) = crate::config::isolated_config_dir();
        for preset in [ProviderPreset::OpenAI, ProviderPreset::Anthropic, ProviderPreset::Gemini,
            ProviderPreset::Groq, ProviderPreset::Cerebras, ProviderPreset::Xai, ProviderPreset::Mistral,
            ProviderPreset::DeepSeek, ProviderPreset::Together, ProviderPreset::Fireworks,
            ProviderPreset::Ollama, ProviderPreset::LmStudio, ProviderPreset::Custom] {
            let server = MockServer::start().await;
            Mock::given(method("GET")).respond_with(ResponseTemplate::new(500)).expect(0).mount(&server).await;
            let mut body = ok_body();
            body["model"] = json!(preset.default_model());
            body["usage"] = json!({"prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500,
                "prompt_tokens_details": {"cached_tokens": 200}});
            Mock::given(method("POST")).and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body)).expect(1).mount(&server).await;
            let provider = OpenRouter::with_provider(preset, "test-key", server.uri()).with_max_attempts(1);
            let mut req = request(vec![], None);
            req.model = preset.default_model().into();
            let response = provider.chat(req).await.unwrap();
            let price = crate::engine::pricing::builtin_price(preset, preset.default_model());
            assert_eq!(response.usage.estimated, price.is_some());
            assert_eq!(response.usage.cost, price.map(|price| price.cost_of(&response.usage, 200)));
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
        std::env::remove_var("SLAG_CONFIG_DIR");
    }

    #[tokio::test]
    async fn missing_token_split_keeps_priced_models_unknown() {
        let server = MockServer::start().await;
        let mut body = ok_body();
        body["model"] = json!("gpt-4.1-mini");
        body["usage"] = json!({"total_tokens": 1000});
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body)).expect(1).mount(&server).await;
        let mut req = request(vec![], None);
        req.model = "gpt-4.1-mini".into();
        let response = OpenRouter::with_provider(ProviderPreset::OpenAI, "test-key", server.uri()).chat(req).await.unwrap();
        assert_eq!(response.usage.cost, None);
        assert_eq!(crate::engine::pricing::format_cost(&response.usage), "spend unknown · 1.0k tokens");
    }

    #[tokio::test]
    async fn configured_price_wins_even_over_reported_cost_and_model_aliases() {
        let (_guard, dir) = crate::config::isolated_config_dir();
        std::fs::write(dir.path().join("config.toml"), "[pricing]\n\"openai::gpt-4.1-mini\" = { input = 2, output = 4, cached_input = 1 }\n").unwrap();
        let server = MockServer::start().await;
        let mut body = ok_body();
        body["model"] = json!("gpt-4.1-mini-2025-04-14");
        body["usage"] = json!({"prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500,
            "cost": 99, "prompt_tokens_details": {"cached_tokens": 200}});
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body)).expect(1).mount(&server).await;
        let mut req = request(vec![], None);
        req.model = "gpt-4.1-mini".into();
        let response = OpenRouter::with_provider(ProviderPreset::OpenAI, "test-key", server.uri()).chat(req).await.unwrap();
        std::env::remove_var("SLAG_CONFIG_DIR");
        assert!(response.usage.estimated);
        assert!((response.usage.cost.unwrap() - 0.0038).abs() < 1e-9);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn credits_skip_network_without_an_account_key() {
        let server = MockServer::start().await;
        assert_eq!(fetch_credits("", &server.uri()).await, None);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn presets_gate_router_fields_headers_and_model_lookups() {
        for preset in [
            ProviderPreset::OpenRouter, ProviderPreset::OpenAI, ProviderPreset::Anthropic,
            ProviderPreset::Gemini, ProviderPreset::Groq, ProviderPreset::Cerebras,
            ProviderPreset::Xai, ProviderPreset::Mistral, ProviderPreset::DeepSeek,
            ProviderPreset::Together, ProviderPreset::Fireworks, ProviderPreset::Ollama,
            ProviderPreset::LmStudio, ProviderPreset::Custom,
        ] {
            let server = MockServer::start().await;
            let mut response = ok_body();
            if preset == ProviderPreset::OpenRouter {
                response["usage"] = json!({"cost": 0.001});
            }
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .expect(1)
                .mount(&server).await;
            let key = if preset.key_optional() { "" } else { "test-key" };
            let provider = OpenRouter::with_provider(preset, key, server.uri())
                .with_fallback(Some("fallback-model")).with_unattended(false);
            let mut req = request(vec![spec()], Some(Effort::High));
            req.model = preset.default_model().into();
            req.messages.push(ChatMessage::assistant("analysis", None)
                .with_reasoning_details(Some(json!([{"text": "router-only"}]))));
            provider.chat(req).await.unwrap();
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1, "{} made an extra request", preset.name());
            let sent = &requests[0];
            let body: Value = serde_json::from_slice(&sent.body).unwrap();
            assert_eq!(body["model"], preset.default_model());
            assert_eq!(body["tools"][0]["function"]["name"], "read_file");
            assert!(body.get("parallel_tool_calls").is_none());
            if preset == ProviderPreset::OpenRouter {
                assert!(sent.headers.contains_key("HTTP-Referer"));
                assert!(sent.headers.contains_key("X-Title"));
                assert!(body.get("usage").is_some());
                assert!(body.get("models").is_some());
                assert!(body.get("reasoning").is_some());
                assert!(body["messages"][2].get("reasoning_details").is_some());
            } else {
                assert!(!sent.headers.contains_key("HTTP-Referer"), "{}", preset.name());
                assert!(!sent.headers.contains_key("X-Title"));
                assert!(body.get("usage").is_none());
                assert!(body.get("models").is_none());
                assert!(body.get("reasoning").is_none());
                assert!(body["messages"][2].get("reasoning_details").is_none());
                assert_eq!(provider.context_length(preset.default_model()).await, None);
                assert_eq!(server.received_requests().await.unwrap().len(), 1);
            }
            assert_eq!(sent.headers.contains_key("Authorization"), !key.is_empty());
        }
    }

    #[tokio::test]
    async fn reasoning_effort_and_token_limits_match_supported_models() {
        for (preset, model, effort) in [
            (ProviderPreset::OpenAI, "gpt-5", Some("medium")),
            (ProviderPreset::OpenAI, "o3-mini", Some("medium")),
            (ProviderPreset::OpenAI, "gpt-4.1-mini", None),
            (ProviderPreset::OpenAI, "gpt-5-chat-latest", None),
            (ProviderPreset::OpenAI, "o1-mini", None),
            (ProviderPreset::Gemini, "gemini-2.5-flash", Some("medium")),
            (ProviderPreset::Gemini, "gemini-3-pro-preview", Some("high")),
            (ProviderPreset::Groq, "openai/gpt-oss-20b", Some("medium")),
            (ProviderPreset::Groq, "llama-3.3-70b-versatile", None),
            (ProviderPreset::Cerebras, "gpt-oss-120b", Some("medium")),
            (ProviderPreset::Cerebras, "llama3.1-8b", None),
            (ProviderPreset::Cerebras, "qwen-3.8-27b", Some("medium")),
            (ProviderPreset::Xai, "grok-3-mini", Some("high")),
            (ProviderPreset::Anthropic, "claude-sonnet-4-5", None),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
                .expect(1).mount(&server).await;
            let provider = OpenRouter::with_provider(preset, "test-key", server.uri());
            let mut req = request(vec![spec()], Some(Effort::Medium));
            req.model = model.into();
            req.max_tokens = Some(32);
            provider.chat(req).await.unwrap();
            let requests = server.received_requests().await.unwrap();
            let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(body.get("reasoning_effort").and_then(Value::as_str), effort, "{model}");
            assert!(body.get("reasoning").is_none());
            let field = if preset == ProviderPreset::OpenAI && openai_reasoning_model(model) {
                assert!(body.get("max_tokens").is_none());
                "max_completion_tokens"
            } else {
                assert!(body.get("max_completion_tokens").is_none());
                "max_tokens"
            };
            assert_eq!(body[field], 32);
        }
    }

    #[tokio::test]
    async fn rejected_tool_choice_retries_once_and_caches_the_quirk() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"tool_choice": "auto"})))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "tool_choice is not supported"}
            })))
            .expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(|r: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&r.body).unwrap();
                body.get("tool_choice").is_none() && body["tools"][0]["function"]["name"] == "read_file"
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(2).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Cerebras, "test-key", server.uri());
        provider.chat(request(vec![spec()], None)).await.unwrap();
        provider.chat(request(vec![spec()], None)).await.unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn rejected_empty_assistant_content_keeps_tool_calls_and_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(|r: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&r.body).unwrap();
                body["messages"][2].get("content").is_some()
            })
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "error": {"message": "assistant content must not be empty with tool_calls"}
            })))
            .expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(|r: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&r.body).unwrap();
                body["messages"][2].get("content").is_none()
                    && body["messages"][2]["tool_calls"][0]["id"] == "call_1"
                    && body["messages"][3]["tool_call_id"] == "call_1"
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Custom, "", server.uri());
        let mut req = request(vec![spec()], None);
        req.messages.push(ChatMessage::assistant("", Some(vec![ToolCall {
            id: "call_1".into(), name: "read_file".into(), arguments: "{}".into(),
        }])));
        req.messages.push(ChatMessage::tool_result("call_1", "ok"));
        provider.chat(req).await.unwrap();
    }

    #[test]
    fn tool_shape_fallback_only_removes_the_rejected_fields() {
        let mut body = body_builder().build_body(&request(vec![spec()], None));
        body["parallel_tool_calls"] = json!(true);
        let tools = body["tools"].clone();
        let quirk = rejected_tool_shape(&body, "unknown parameter parallel_tool_calls").unwrap();
        apply_quirks(&mut body, quirk);
        assert!(body.get("parallel_tool_calls").is_none());
        assert_eq!(body["tools"], tools);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(rejected_tool_shape(&body, "invalid tool argument"), None);
        assert_eq!(rejected_tool_shape(&body, "unknown parameter parallel_tool_calls"), None);
    }

    #[tokio::test]
    async fn tool_validation_errors_return_feedback_without_transport_retries() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {
                    "message": "Tool call validation failed: missing pattern secret-test-key",
                    "code": "tool_use_failed",
                    "failed_generation": "{\"name\":\"grep\",\"arguments\":{}}"
                }
            })))
            .expect(1).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Groq, "secret-test-key", server.uri());
        let mut finish = spec();
        finish.name = "finish".into();
        let error = provider.chat(request(vec![spec(), finish], None)).await.unwrap_err();
        let SlagError::ToolValidation(message) = error else { panic!("expected tool validation error") };
        assert!(message.contains("missing pattern [redacted]"));
        assert!(message.contains("grep"));
        assert!(message.contains("Use only these declared tool names: read_file, finish."));
        assert!(message.contains("To complete the task, call finish with cmd and summary."));
        assert!(!message.contains("secret-test-key"));
    }

    #[tokio::test]
    async fn plain_text_tool_validation_errors_return_feedback() {
        for message in ["Tool call validation failed: missing pattern", "Failed to call a function: missing pattern"] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(400).set_body_string(message))
                .expect(1).mount(&server).await;
            let provider = OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri());
            let error = provider.chat(request(vec![spec()], None)).await.unwrap_err();
            let SlagError::ToolValidation(feedback) = error else { panic!("expected tool validation error") };
            assert!(feedback.contains(message));
            assert!(feedback.contains("groq, role smith, model qwen/qwen3-coder"));
        }
        assert!(!is_tool_validation_error("invalid tool schema: missing required property"));
    }

    #[tokio::test]
    async fn tool_validation_requires_declared_tools_and_a_400() {
        for (status, tools) in [(400, vec![]), (401, vec![spec()]), (422, vec![spec()])] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "error": {"message": "Tool call validation failed", "code": "tool_use_failed"}
                })))
                .expect(1).mount(&server).await;
            let provider = OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri());
            assert!(matches!(provider.chat(request(tools, None)).await.unwrap_err(), SlagError::ProviderApi(_)));
        }
    }

    #[tokio::test]
    async fn authenticated_requests_do_not_follow_redirects() {
        let server = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(307).insert_header("Location", destination.uri()))
            .expect(1).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Groq, "secret-test-key", server.uri());
        assert!(provider.chat(request(vec![], None)).await.is_err());
        assert!(destination.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unrelated_invalid_requests_fail_without_shape_retries() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "invalid tool schema: missing required property"}
            })))
            .expect(1).mount(&server).await;
        let error = OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri())
            .chat(request(vec![spec()], None)).await.unwrap_err();
        assert!(error.to_string().contains("invalid tool schema: missing required property"));
    }

    #[tokio::test]
    async fn rejected_shapes_respect_the_side_request_budget() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "tool_choice is not supported"}
            })))
            .expect(1).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri());
        let mut req = request(vec![spec()], None);
        req.retry = RetryPolicy::side();
        let error = provider.chat(req).await.unwrap_err();
        assert!(error.to_string().contains("tool_choice is not supported"));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn repeated_shape_rejections_stop_after_the_field_is_removed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "tool_choice is not supported"}
            })))
            .expect(2).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri());
        let error = provider.chat(request(vec![spec()], None)).await.unwrap_err();
        assert!(error.to_string().contains("tool_choice is not supported"));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn generic_errors_show_readable_messages_and_redact_keys() {
        for status in [401, 402, 422] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "error": {"message": "rejected secret-test-key: check the account"}
                })))
                .expect(1).mount(&server).await;
            let error = OpenRouter::with_provider(ProviderPreset::OpenAI, "secret-test-key", server.uri())
                .chat(request(vec![], None)).await.unwrap_err().to_string();
            assert!(error.contains("rejected [redacted]: check the account"), "{error}");
            assert!(!error.contains("secret-test-key"));
            assert!(!error.contains("OpenRouter"));
            assert!(!error.contains("openrouter.ai"));
            assert!(!error.contains("\"error\""));
        }
    }

    #[tokio::test]
    async fn qualified_model_strips_only_the_explicit_provider_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"model": "openai/gpt-oss-20b"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1).mount(&server).await;
        let mut req = request(vec![spec()], None);
        req.model = "groq::openai/gpt-oss-20b".into();
        OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri())
            .chat(req).await.unwrap();
    }

    #[tokio::test]
    async fn qualified_response_names_suppress_false_routing_and_namespace_usage() {
        for returned in ["gpt-4.1-mini", "gpt-4.1-mini-2025-04-14"] {
            let server = MockServer::start().await;
            let mut body = ok_body();
            body["model"] = json!(returned);
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1).mount(&server).await;
            let mut req = request(vec![], None);
            req.model = "openai::gpt-4.1-mini".into();
            let response = OpenRouter::with_provider(ProviderPreset::OpenAI, "test-key", server.uri())
                .chat(req).await.unwrap();
            let displayed = if returned == "gpt-4.1-mini" { "openai::gpt-4.1-mini" } else { returned };
            assert_eq!(response.model.as_deref(), Some(displayed));
            assert_eq!(response.usage.model, Some(format!("openai::{returned}")));
        }
    }

    #[tokio::test]
    async fn native_thinking_metadata_survives_tool_round_trips() {
        for preset in [ProviderPreset::DeepSeek, ProviderPreset::Gemini] {
            let server = MockServer::start().await;
            let mut message = json!({
                "role": "assistant", "content": null,
                "tool_calls": [
                    {"type": "function", "function": {"name": "read_file", "arguments": "{}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "read_file", "arguments": "{}"}},
                ],
            });
            if preset == ProviderPreset::DeepSeek {
                message["reasoning_content"] = json!("Inspect both files first.");
            } else {
                message["tool_calls"][0]["extra_content"] = json!({"google": {"thought_signature": "opaque-signature"}});
                message["extra_content"] = json!({"google": {"thought_signature": "message-signature"}});
            }
            Mock::given(method("POST"))
                .and(|r: &wiremock::Request| {
                    let body: Value = serde_json::from_slice(&r.body).unwrap();
                    body["messages"].as_array().unwrap().len() == 2
                })
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{"message": message, "finish_reason": "tool_calls"}]
                })))
                .expect(1).mount(&server).await;
            Mock::given(method("POST"))
                .and(move |r: &wiremock::Request| {
                    let body: Value = serde_json::from_slice(&r.body).unwrap();
                    let assistant = &body["messages"][2];
                    if assistant.get("reasoning_details").is_some()
                        || assistant["tool_calls"].as_array().is_none_or(|calls| calls.len() != 2) {
                        return false;
                    }
                    if preset == ProviderPreset::DeepSeek {
                        assistant["reasoning_content"] == "Inspect both files first."
                            && assistant.get("extra_content").is_none()
                    } else {
                        assistant.get("reasoning_content").is_none()
                            && assistant["tool_calls"][0]["extra_content"]["google"]["thought_signature"] == "opaque-signature"
                            && assistant["tool_calls"][1].get("extra_content").is_none()
                            && assistant["extra_content"]["google"]["thought_signature"] == "message-signature"
                    }
                })
                .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
                .expect(1).mount(&server).await;
            let provider = OpenRouter::with_provider(preset, "test-key", server.uri());
            let first = provider.chat(request(vec![spec()], None)).await.unwrap();
            let assistant = ChatMessage::assistant(first.content, Some(first.tool_calls.clone()))
                .with_reasoning_details(first.reasoning_details.clone());
            let mut followup = request(vec![spec()], None);
            followup.messages.push(assistant.clone());
            for call in &first.tool_calls {
                followup.messages.push(ChatMessage::tool_result(&call.id, "file text"));
            }
            let result = provider.chat(followup).await.unwrap();
            assert_eq!(result.content, "forged");
            let mut other_request = request(vec![spec()], None);
            other_request.messages.push(assistant);
            for other_provider in [ProviderPreset::OpenAI, ProviderPreset::OpenRouter] {
                let other = OpenRouter::with_provider(other_provider, "test-key", "http://localhost:0")
                    .build_body(&other_request);
                assert!(other["messages"][2].get("reasoning_details").is_none());
                assert!(other["messages"][2].get("reasoning_content").is_none());
                assert!(other["messages"][2].get("extra_content").is_none());
                assert!(other["messages"][2]["tool_calls"][0].get("extra_content").is_none());
            }
        }
    }

    #[tokio::test]
    async fn generic_retry_after_waits_and_stops_at_the_attempt_budget() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1")
                .set_body_json(json!({"error": {"message": "rate limit"}})))
            .expect(2).mount(&server).await;
        let provider = OpenRouter::with_provider(ProviderPreset::Groq, "test-key", server.uri())
            .with_max_attempts(2);
        let started = std::time::Instant::now();
        let error = provider.chat(request(vec![spec()], None)).await.unwrap_err();
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(error.to_string().contains("gave up after 2 attempts"));
        assert!(error.to_string().contains("rate limit"));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[test]
    fn retry_after_http_dates_use_the_request_clock_and_cap_the_delay() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap());
        let at = std::time::UNIX_EPOCH + Duration::from_secs(1_445_412_480);
        assert_eq!(parse_retry_after_at(&headers, at - Duration::from_secs(5)), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after_at(&headers, at - Duration::from_secs(90)), Some(Duration::from_secs(60)));
        assert_eq!(parse_retry_after_at(&headers, at + Duration::from_secs(1)), Some(Duration::ZERO));
        headers.insert("retry-after", "invalid".parse().unwrap());
        assert_eq!(parse_retry_after_at(&headers, at), None);
    }

    #[test]
    fn routed_clients_reuse_connections_and_quirks_and_receive_sinks() {
        let provider = body_builder();
        let first = provider.routed_client(ProviderPreset::Groq, "test-key".into(), "http://localhost:1".into());
        first.quirks.store(OMIT_TOOL_CHOICE, std::sync::atomic::Ordering::Relaxed);
        let second = provider.routed_client(ProviderPreset::Groq, "test-key".into(), "http://localhost:1".into());
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(second.quirks.load(std::sync::atomic::Ordering::Relaxed), OMIT_TOOL_CHOICE);
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        provider.set_event_sink(tx);
        provider.set_cancel_flag(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        assert!(first.has_event_sink());
        assert!(first.has_cancel_flag());
    }

    #[tokio::test]
    #[ignore = "uses provider keys and spends a small amount on seven tool calls"]
    async fn live_provider_tool_smoke() {
        for preset in [
            ProviderPreset::OpenRouter, ProviderPreset::OpenAI, ProviderPreset::Cerebras,
            ProviderPreset::Groq, ProviderPreset::Gemini, ProviderPreset::Xai, ProviderPreset::Mistral,
        ] {
            let Some(key) = preset.key_env().and_then(|env| std::env::var(env).ok()).filter(|key| !key.trim().is_empty()) else {
                println!("SMOKE {} SKIP key absent", preset.name());
                continue;
            };
            let provider = OpenRouter::with_provider(preset, key, preset.base_url())
                .with_max_attempts(1).with_timeout(Duration::from_secs(60))
                .with_unattended(false).with_fallback(None);
            let tool = ToolSpec {
                name: "ping".into(), description: "Return the supplied value".into(),
                parameters: json!({"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}),
            };
            let model = if preset == ProviderPreset::OpenRouter { "openai/gpt-4.1-mini" } else { preset.default_model() };
            let req = ChatRequest {
                model: model.into(),
                messages: vec![ChatMessage::user("Call ping with value pong. Use the tool once. Do not answer with text.")],
                tools: vec![tool], effort: None, max_tokens: Some(256),
                role: crate::engine::Role::Smith, retry: RetryPolicy::side(),
            };
            let started = std::time::Instant::now();
            match provider.chat(req).await {
                Ok(response) if response.tool_calls.iter().any(|call| call.name == "ping") => {
                    println!("SMOKE {} PASS {}ms", preset.name(), started.elapsed().as_millis());
                }
                Ok(_) => println!("SMOKE {} FAIL no tool call {}ms", preset.name(), started.elapsed().as_millis()),
                Err(error) => println!("SMOKE {} FAIL {}ms {}", preset.name(), started.elapsed().as_millis(), error),
            }
        }
    }

}
