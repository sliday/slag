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

    /// Enable branch-per-ingot worktree isolation
    #[arg(long)]
    pub worktree: bool,

    /// Max parallel anvil workers
    #[arg(long, default_value_t = crate::config::MAX_ANVILS)]
    pub anvils: usize,
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
