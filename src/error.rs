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

    #[error("review failed: {0} branches rejected")]
    ReviewFailed(usize),

    #[error("CI check failed for branch {branch}: {reason}")]
    CiFailed { branch: String, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
