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

    /// Branch-per-ingot worktree isolation (not implemented yet; ignored
    /// with a warning — all ingots run in the shared checkout)
    #[arg(long)]
    pub worktree: bool,

    /// Full-screen dashboard (crucible, live feed, steering input)
    #[arg(long)]
    pub tui: bool,

    /// Max parallel anvil workers
    #[arg(long, default_value_t = crate::config::MAX_ANVILS)]
    pub anvils: usize,

    /// Let OpenRouter pick the model per call: openrouter/auto for
    /// worker, planner, and judge (duel cast B keeps its own model
    /// for diversity). Explicit model flags override.
    #[arg(long)]
    pub auto: bool,

    /// Worker model, any OpenRouter id (overrides SLAG_MODEL_BASE)
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Planner model for grade>=3 ingots (overrides SLAG_MODEL_PLAN)
    #[arg(long, value_name = "MODEL")]
    pub plan_model: Option<String>,

    /// Duel judge model (overrides SLAG_MODEL_JUDGE)
    #[arg(long, value_name = "MODEL")]
    pub judge_model: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show or set the OpenRouter key (the only setup slag needs)
    Key {
        /// Key to verify and save; omit to show the current setup
        #[arg(value_name = "KEY")]
        key: Option<String>,
    },

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_flag_still_parses_and_help_says_it_is_ignored() {
        let cli = Cli::parse_from(["slag", "--worktree", "build", "it"]);
        assert!(cli.worktree);
        assert_eq!(cli.commission_text().as_deref(), Some("build it"));

        // The advertised behavior must match reality: the help text says
        // the flag is ignored until the isolation is actually wired up.
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("not implemented yet"), "help: {help}");
    }
}
