use std::path::PathBuf;

/// File paths used by the pipeline
pub const BLUEPRINT: &str = "BLUEPRINT.md";
pub const CRUCIBLE: &str = "PLAN.md";
pub const ORE_FILE: &str = "PRD.md";
pub const ALLOY_FILE: &str = "AGENTS.md";
pub const LEDGER: &str = "PROGRESS.md";
pub const LOG_DIR: &str = "logs";

/// Behavior constants
pub const MAX_ANVILS: usize = 3;
pub const HIGH_GRADE: u8 = 3;
pub const MAX_ITERATE: usize = 3;

/// Smith configuration resolved from environment
pub struct SmithConfig {
    pub base: String,
    pub plan: String,
    pub web: String,
    pub web_plan: String,
}

impl SmithConfig {
    pub fn from_env() -> Self {
        let base = std::env::var("SLAG_SMITH")
            .unwrap_or_else(|_| "claude --dangerously-skip-permissions -p".to_string());
        let plan = format!("{base} --permission-mode plan");
        let web = format!("{base} --allowedTools 'Bash Edit Read Write Playwright'");
        let web_plan = format!("{web} --permission-mode plan");
        Self {
            base,
            plan,
            web,
            web_plan,
        }
    }

    /// Select smith command based on skill and grade
    pub fn select(&self, skill: &str, grade: u8) -> &str {
        match skill {
            "web" | "frontend" | "ui" | "css" | "html" => {
                if grade >= HIGH_GRADE {
                    &self.web_plan
                } else {
                    &self.web
                }
            }
            _ => {
                if grade >= HIGH_GRADE {
                    &self.plan
                } else {
                    &self.base
                }
            }
        }
    }
}

/// Resolve a project-relative path
pub fn project_path(filename: &str) -> PathBuf {
    PathBuf::from(filename)
}

/// Pipeline execution configuration (from CLI flags)
#[derive(Debug, Clone, Default)]
pub struct PipelineConfig {
    /// Enable worktree isolation per ingot
    pub worktree: bool,
    /// Max parallel anvils
    pub max_anvils: usize,
    /// Skip the review phase
    pub skip_review: bool,
    /// Keep branches after review
    pub keep_branches: bool,
    /// CI checks only, no AI review
    pub ci_only: bool,
    /// Review even if CI fails
    pub review_all: bool,
}

impl PipelineConfig {
    pub fn new(
        worktree: bool,
        max_anvils: usize,
        skip_review: bool,
        keep_branches: bool,
        ci_only: bool,
        review_all: bool,
    ) -> Self {
        Self {
            worktree,
            max_anvils,
            skip_review,
            keep_branches,
            ci_only,
            review_all,
        }
    }

    /// Check if review phase should run
    pub fn should_review(&self) -> bool {
        self.worktree && !self.skip_review
    }
}
