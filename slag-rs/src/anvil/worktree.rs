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

    // Reclaim leftovers from an interrupted run: the names are
    // deterministic, so a stale worktree dir or forge/* branch would
    // otherwise block every future duel of this ingot. All best-effort —
    // `git worktree add` below has the final word.
    let _ = tokio::process::Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo)
        .output()
        .await;
    discard_in(repo, ingot_id).await;
    let leftover = repo.join(&dir);
    if tokio::fs::metadata(&leftover).await.is_ok() {
        let _ = tokio::fs::remove_dir_all(&leftover).await;
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh git repo nested inside a tempdir so `../slag-anvil-*`
    /// worktrees stay contained.
    fn test_repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "forge@slag.test"],
            vec!["config", "user.name", "slag"],
            vec!["commit", "--allow-empty", "-m", "base"],
        ] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }
        (tmp, repo)
    }

    #[tokio::test]
    async fn create_in_reclaims_stale_worktree_and_branch() {
        let (_tmp, repo) = test_repo();
        // Simulate an interrupted run: worktree + branch left behind, dirty.
        let dir = create_in(&repo, "i3-r1a").await.unwrap();
        std::fs::write(dir.join("wip.txt"), "half-done\n").unwrap();

        // A resume re-runs round 1 under the same deterministic names.
        let dir2 = create_in(&repo, "i3-r1a")
            .await
            .expect("stale worktree/branch must be reclaimed");
        assert!(!dir2.join("wip.txt").exists(), "fresh worktree, no stale state");
        discard_in(&repo, "i3-r1a").await;
    }

    #[tokio::test]
    async fn create_in_reclaims_manually_deleted_worktree() {
        let (_tmp, repo) = test_repo();
        let dir = create_in(&repo, "i4-r1a").await.unwrap();
        // Dir gone, but the registration and forge/* branch remain.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(create_in(&repo, "i4-r1a").await.is_ok());
        discard_in(&repo, "i4-r1a").await;
    }
}
