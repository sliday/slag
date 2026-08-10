use std::path::{Path, PathBuf};

use crate::error::SlagError;

fn branch_name(ingot_id: &str) -> String {
    format!("forge/{ingot_id}")
}

fn dir_name(ingot_id: &str) -> String {
    format!("../slag-anvil-{ingot_id}")
}

/// Create a git worktree for an ingot's isolated execution
pub async fn create(ingot_id: &str) -> Result<String, SlagError> {
    create_in(Path::new("."), ingot_id)
        .await
        .map(|p| p.to_string_lossy().into_owned())
}

/// Repo-aware variant: run `git worktree add` from `repo`, returning the
/// worktree path joined onto it. Same branch/dir naming as `create`.
pub async fn create_in(repo: &Path, ingot_id: &str) -> Result<PathBuf, SlagError> {
    let branch = branch_name(ingot_id);
    let dir = dir_name(ingot_id);

    let output = tokio::process::Command::new("git")
        .args(["worktree", "add", &dir, "-b", &branch])
        .current_dir(repo)
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlagError::WorktreeError(format!(
            "worktree add failed: {stderr}"
        )));
    }

    Ok(repo.join(dir))
}

/// Merge a worktree branch back to main and clean up
pub async fn merge_and_cleanup(ingot_id: &str) -> Result<(), SlagError> {
    merge_and_cleanup_in(Path::new("."), ingot_id).await
}

/// Repo-aware variant of `merge_and_cleanup`: merge into the branch checked
/// out at `repo`, then remove the worktree and delete the branch.
pub async fn merge_and_cleanup_in(repo: &Path, ingot_id: &str) -> Result<(), SlagError> {
    let branch = branch_name(ingot_id);
    let dir = dir_name(ingot_id);

    // Merge
    let output = tokio::process::Command::new("git")
        .args(["merge", &branch])
        .current_dir(repo)
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("merge failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // A conflicted merge leaves MERGE_HEAD + conflict markers in the
        // main checkout; abort so fallback paths never commit them.
        let _ = tokio::process::Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(repo)
            .output()
            .await;
        return Err(SlagError::WorktreeError(format!(
            "merge {branch} failed: {stderr}"
        )));
    }

    // Remove worktree
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "remove", &dir])
        .current_dir(repo)
        .output()
        .await;

    // Delete branch
    let _ = tokio::process::Command::new("git")
        .args(["branch", "-d", &branch])
        .current_dir(repo)
        .output()
        .await;

    Ok(())
}

/// Remove a worktree without merging (failure case, preserves branch for debugging)
pub async fn cleanup_without_merge(ingot_id: &str) {
    cleanup_without_merge_in(Path::new("."), ingot_id).await
}

/// Repo-aware variant of `cleanup_without_merge`.
pub async fn cleanup_without_merge_in(repo: &Path, ingot_id: &str) {
    let dir = dir_name(ingot_id);
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "remove", "--force", &dir])
        .current_dir(repo)
        .output()
        .await;
}

/// Discard a duel cast entirely: remove the worktree AND its branch.
/// Duel rounds mint fresh branch names per round, but losers must not
/// litter the repo with dead `forge/*` branches.
pub async fn discard_in(repo: &Path, ingot_id: &str) {
    cleanup_without_merge_in(repo, ingot_id).await;
    let _ = tokio::process::Command::new("git")
        .args(["branch", "-D", &branch_name(ingot_id)])
        .current_dir(repo)
        .output()
        .await;
}
