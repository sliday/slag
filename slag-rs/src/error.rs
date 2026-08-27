use thiserror::Error;

/// Provider failure classes, assigned once where the HTTP response is in
/// hand (`engine::provider`). The dashboard and JSONL logs show the human
/// label instead of a raw body excerpt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorCategory {
    /// 401/403 — the key itself was refused.
    Auth,
    /// 402 — the account has no credit.
    Billing,
    /// 429 — rate limited.
    RateLimit,
    /// 529/503-style capacity shedding.
    Overloaded,
    /// 400/404/422 — the request itself is wrong.
    InvalidRequest,
    /// Other 5xx.
    Server,
    /// Connect/timeout/reset before any HTTP status arrived.
    Network,
    /// A 200 whose body could not be used (malformed or empty).
    BadResponse,
}

impl ProviderErrorCategory {
    /// Human-readable label for dashboards and logs.
    pub fn label(&self) -> &'static str {
        match self {
            ProviderErrorCategory::Auth => "invalid key",
            ProviderErrorCategory::Billing => "credit balance low",
            ProviderErrorCategory::RateLimit => "rate limited",
            ProviderErrorCategory::Overloaded => "overloaded",
            ProviderErrorCategory::InvalidRequest => "invalid request",
            ProviderErrorCategory::Server => "server error",
            ProviderErrorCategory::Network => "network error",
            ProviderErrorCategory::BadResponse => "malformed response",
        }
    }
}

/// One classified provider failure. Classification happens exactly once,
/// in `engine::provider`; everything downstream branches on the fields
/// instead of grepping a message string.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderApiError {
    /// HTTP status, when one arrived.
    pub status: Option<u16>,
    pub category: ProviderErrorCategory,
    /// Whether retrying the same call may succeed.
    pub retryable: bool,
    /// Body excerpt (plus any remedy text) kept for bug reports.
    pub excerpt: String,
}

impl std::fmt::Display for ProviderApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.category.label())?;
        if let Some(status) = self.status {
            write!(f, " ({status})")?;
        }
        if !self.excerpt.is_empty() {
            write!(f, ": {}", self.excerpt)?;
        }
        Ok(())
    }
}

#[derive(Error, Debug)]
pub enum SlagError {
    #[error("no PRD.md found — provide a commission")]
    NoOre,

    #[error("surveyor failed: {0}")]
    SurveyFailed(String),

    #[error("founder failed: {0}")]
    FounderFailed(String),

    #[error("smith invocation failed: {0}")]
    SmithFailed(String),

    #[error("interrupted by user")]
    Cancelled,

    /// Run-wide spend cap (`SLAG_MAX_COST_RUN`) tripped mid-flight. Not a
    /// smith failure: the ingot goes back to ore and the run stops cleanly
    /// so `slag resume` can pick it up under a raised cap.
    #[error("run budget exhausted (${spent:.2} of ${cap:.2} cap)")]
    RunBudgetExhausted { spent: f64, cap: f64 },

    #[error("no ingots produced by founder")]
    NoIngots,

    /// The forge finished and the warden judged the commission unmet. The
    /// tasks passed; the goal did not. Reported as a failure on purpose:
    /// a run that prints FORGED over an unmet goal is worse than one that
    /// never checked, because it launders the doubt.
    #[error("goal not met: {0}")]
    GoalNotMet(String),

    #[error("crucible parse error: {0}")]
    CrucibleParse(String),

    #[error("ingot {0} cracked after {1} heats")]
    IngotCracked(String, u8),

    #[error("forge failed: {0} ingots cracked")]
    ForgeFailed(usize),

    #[error("proof failed for {id}: {reason}")]
    ProofFailed { id: String, reason: String },

    #[error("self-update failed: {0}")]
    UpdateFailed(String),

    #[error("worktree error: {0}")]
    WorktreeError(String),

    #[error("provider error: {0}")]
    Provider(String),

    /// Classified provider failure ({status, category, retryable,
    /// excerpt}), built once in `engine::provider`. The category label
    /// rides the Display string, so the existing `Error` event message
    /// shows "credit balance low" instead of a raw body excerpt.
    #[error("provider error: {0}")]
    ProviderApi(ProviderApiError),

    /// Transient provider failure (rate limit, 5xx, dropped connection,
    /// empty completion). Worth retrying; `Provider` is permanent.
    #[error("provider error (transient): {0}")]
    ProviderTransient(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("config error: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SlagError {
    /// True when retrying the same call may succeed. Permanent failures
    /// (auth, billing, malformed requests) return false.
    pub fn retryable(&self) -> bool {
        match self {
            SlagError::ProviderTransient(_) => true,
            SlagError::ProviderApi(e) => e.retryable,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_is_retryable_permanent_is_not() {
        assert!(SlagError::ProviderTransient("429: slow down".into()).retryable());
        assert!(!SlagError::Provider("401: bad key".into()).retryable());
        assert!(!SlagError::Cancelled.retryable());
    }

    fn api_err(
        status: Option<u16>,
        category: ProviderErrorCategory,
        retryable: bool,
        excerpt: &str,
    ) -> ProviderApiError {
        ProviderApiError { status, category, retryable, excerpt: excerpt.into() }
    }

    #[test]
    fn typed_provider_error_carries_its_retryable_flag() {
        let transient = api_err(Some(429), ProviderErrorCategory::RateLimit, true, "slow down");
        assert!(SlagError::ProviderApi(transient).retryable());
        let permanent = api_err(Some(401), ProviderErrorCategory::Auth, false, "bad key");
        assert!(!SlagError::ProviderApi(permanent).retryable());
    }

    /// The category label leads the Display string — that is what the
    /// dashboard's Error event shows instead of a raw body excerpt.
    #[test]
    fn typed_provider_error_display_leads_with_the_category() {
        let err = SlagError::ProviderApi(api_err(
            Some(402),
            ProviderErrorCategory::Billing,
            false,
            "Top up at https://openrouter.ai/credits",
        ));
        let msg = err.to_string();
        assert!(msg.contains("credit balance low"), "{msg}");
        assert!(msg.contains("(402)"), "{msg}");
        assert!(msg.contains("openrouter.ai/credits"), "{msg}");

        // No status (network-class) and no excerpt still render cleanly.
        let bare = api_err(None, ProviderErrorCategory::Network, true, "");
        assert_eq!(bare.to_string(), "network error");
    }

    #[test]
    fn category_labels_are_human_readable() {
        for (category, label) in [
            (ProviderErrorCategory::Auth, "invalid key"),
            (ProviderErrorCategory::Billing, "credit balance low"),
            (ProviderErrorCategory::RateLimit, "rate limited"),
            (ProviderErrorCategory::Overloaded, "overloaded"),
            (ProviderErrorCategory::InvalidRequest, "invalid request"),
            (ProviderErrorCategory::Server, "server error"),
            (ProviderErrorCategory::Network, "network error"),
            (ProviderErrorCategory::BadResponse, "malformed response"),
        ] {
            assert_eq!(category.label(), label);
        }
    }

    /// Categories serialize snake_case so the JSONL stream stays greppable.
    #[test]
    fn category_serializes_snake_case() {
        let v = serde_json::to_value(ProviderErrorCategory::RateLimit).unwrap();
        assert_eq!(v, serde_json::json!("rate_limit"));
        let v = serde_json::to_value(ProviderErrorCategory::BadResponse).unwrap();
        assert_eq!(v, serde_json::json!("bad_response"));
    }
}
