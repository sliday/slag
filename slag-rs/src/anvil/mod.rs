pub mod worktree;

// The parallel anvil logic is integrated directly into pipeline/forge.rs
// using tokio::task::JoinSet. This module exists for the worktree feature
// and any future anvil-specific coordination logic.
