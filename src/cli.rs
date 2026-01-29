use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "slag",
    about = "Smelt ideas, skim the bugs, forge the product.",
    version,
    author
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Commission (new project description)
    #[arg(trailing_var_arg = true)]
    pub commission: Vec<String>,

    /// Enable branch-per-ingot worktree isolation with master review
    #[arg(long)]
    pub worktree: bool,

    /// Max parallel anvil workers
    #[arg(long, default_value_t = crate::config::MAX_ANVILS)]
    pub anvils: usize,

    /// Skip the master review phase (legacy behavior)
    #[arg(long)]
    pub skip_review: bool,

    /// Don't delete branches after review
    #[arg(long)]
    pub keep_branches: bool,

    /// Run CI checks but skip AI review
    #[arg(long)]
    pub ci_only: bool,

    /// Review even if CI fails
    #[arg(long)]
    pub review_all: bool,

    /// Max retry cycles when ingots crack (0 = no retry)
    #[arg(long, default_value_t = 3)]
    pub retry: usize,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show crucible state
    Status,

    /// Resume existing forge
    Resume,

    /// Self-update to latest release
    Update,
}

impl Cli {
    pub fn commission_text(&self) -> Option<String> {
        if self.commission.is_empty() {
            None
        } else {
            Some(self.commission.join(" "))
        }
    }
}
