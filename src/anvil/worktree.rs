use crate::error::SlagError;

/// Create a git worktree for an ingot's isolated execution
pub async fn create(ingot_id: &str) -> Result<String, SlagError> {
    let branch = format!("forge/{ingot_id}");
    let dir = format!("../slag-anvil-{ingot_id}");

    let output = tokio::process::Command::new("git")
        .args(["worktree", "add", &dir, "-b", &branch])
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlagError::WorktreeError(format!(
            "worktree add failed: {stderr}"
        )));
    }

    Ok(dir)
}

/// Merge a worktree branch back to main and clean up
pub async fn merge_and_cleanup(ingot_id: &str) -> Result<(), SlagError> {
    let branch = format!("forge/{ingot_id}");
    let dir = format!("../slag-anvil-{ingot_id}");

    // Merge
    let output = tokio::process::Command::new("git")
        .args(["merge", &branch])
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("merge failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlagError::WorktreeError(format!(
            "merge {branch} failed: {stderr}"
        )));
    }

    // Remove worktree
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "remove", &dir])
        .output()
        .await;

    // Delete branch
    let _ = tokio::process::Command::new("git")
        .args(["branch", "-d", &branch])
        .output()
        .await;

    Ok(())
}

/// Remove a worktree without merging (failure case, preserves branch for debugging)
pub async fn cleanup_without_merge(ingot_id: &str) {
    let dir = format!("../slag-anvil-{ingot_id}");
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "remove", "--force", &dir])
        .output()
        .await;
}
