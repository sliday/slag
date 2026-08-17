use thiserror::Error;

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
        matches!(self, SlagError::ProviderTransient(_))
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
}
