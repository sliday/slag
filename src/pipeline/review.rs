use crate::config::PipelineConfig;
use crate::error::SlagError;
use crate::flux;
use crate::smith::Smith;
use crate::tui;

use super::forge::ForgeResult;

/// CI check results for a branch
#[derive(Debug, Clone)]
pub struct CiResult {
    pub fmt_passed: bool,
    pub fmt_output: String,
    pub clippy_passed: bool,
    pub clippy_output: String,
    pub test_passed: bool,
    pub test_output: String,
}

impl CiResult {
    pub fn passed(&self) -> bool {
        self.fmt_passed && self.clippy_passed && self.test_passed
    }

    pub fn summary(&self) -> String {
        let fmt = if self.fmt_passed { "✓" } else { "✗" };
        let clippy = if self.clippy_passed { "✓" } else { "✗" };
        let test = if self.test_passed { "✓" } else { "✗" };
        format!("fmt:{fmt} clippy:{clippy} test:{test}")
    }
}

/// Master review result
#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub approved: bool,
    pub comments: String,
}

/// Phase 3.5: Review — master agent quality gate
pub async fn run(
    smith: &dyn Smith,
    config: &PipelineConfig,
    forged_results: &[ForgeResult],
) -> Result<(), SlagError> {
    tui::header("REVIEW · master agent quality gate");

    let branches: Vec<&ForgeResult> = forged_results
        .iter()
        .filter(|r| r.branch.is_some())
        .collect();

    if branches.is_empty() {
        println!("  \x1b[90mNo branches to review\x1b[0m");
        return Ok(());
    }

    println!(
        "  \x1b[38;5;208m⚖\x1b[0m Reviewing {} branches",
        branches.len()
    );

    let mut approved_count = 0;
    let mut rejected_count = 0;

    for forge_result in branches {
        let branch = forge_result.branch.as_ref().unwrap();
        let worktree_path = forge_result.worktree_path.as_deref();

        println!(
            "\n  \x1b[1;37m[{}]\x1b[0m branch: \x1b[90m{}\x1b[0m",
            forge_result.id, branch
        );

        // Run CI checks
        let ci_result = run_ci_checks(branch, worktree_path).await;
        println!("    CI: {}", ci_result.summary());

        if !ci_result.passed() {
            print_ci_failures(&ci_result);
            if !config.review_all {
                println!("    \x1b[31m✗\x1b[0m skipping AI review (CI failed)");
                rejected_count += 1;
                cleanup_branch(&forge_result.id, config.keep_branches).await;
                continue;
            }
        }

        // Skip AI review if ci_only mode
        if config.ci_only {
            if ci_result.passed() {
                println!("    \x1b[38;5;220m◐\x1b[0m CI passed, merging (--ci-only)");
                merge_branch(&forge_result.id).await?;
                approved_count += 1;
            } else {
                rejected_count += 1;
                cleanup_branch(&forge_result.id, config.keep_branches).await;
            }
            continue;
        }

        // Get diff for AI review
        let diff = get_branch_diff(branch).await;

        // Master agent review
        let spinner = tui::spinner("reviewing...");
        let review = master_review(smith, &forge_result.id, branch, &diff, &ci_result).await;
        spinner.finish_and_clear();

        match review {
            Ok(result) => {
                if result.approved {
                    println!("    \x1b[1;37m█\x1b[0m approved");
                    if !result.comments.is_empty() {
                        println!("    \x1b[90m{}\x1b[0m", tui::truncate(&result.comments, 60));
                    }
                    merge_branch(&forge_result.id).await?;
                    approved_count += 1;
                } else {
                    println!("    \x1b[31m✗\x1b[0m rejected");
                    println!("    \x1b[90m{}\x1b[0m", tui::truncate(&result.comments, 60));
                    rejected_count += 1;
                    cleanup_branch(&forge_result.id, config.keep_branches).await;
                }
            }
            Err(e) => {
                eprintln!("    \x1b[31m✗\x1b[0m review error: {e}");
                rejected_count += 1;
                cleanup_branch(&forge_result.id, config.keep_branches).await;
            }
        }
    }

    // Summary
    println!();
    println!(
        "  \x1b[38;5;220m⚖\x1b[0m Review complete: \x1b[1;37m{}\x1b[0m approved, \x1b[31m{}\x1b[0m rejected",
        approved_count, rejected_count
    );

    if rejected_count > 0 && approved_count == 0 {
        return Err(SlagError::ReviewFailed(rejected_count));
    }

    Ok(())
}

/// Run CI checks on a branch
async fn run_ci_checks(branch: &str, worktree_path: Option<&str>) -> CiResult {
    let dir = worktree_path.unwrap_or(".");

    // Checkout branch if in main repo
    if worktree_path.is_none() {
        let _ = tokio::process::Command::new("git")
            .args(["checkout", branch])
            .output()
            .await;
    }

    // cargo fmt --check
    let fmt_output = tokio::process::Command::new("cargo")
        .args(["fmt", "--check"])
        .current_dir(dir)
        .output()
        .await;
    let (fmt_passed, fmt_output) = match fmt_output {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (false, format!("spawn error: {e}")),
    };

    // cargo clippy
    let clippy_output = tokio::process::Command::new("cargo")
        .args(["clippy", "--", "-D", "warnings"])
        .current_dir(dir)
        .output()
        .await;
    let (clippy_passed, clippy_output) = match clippy_output {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (false, format!("spawn error: {e}")),
    };

    // cargo test
    let test_output = tokio::process::Command::new("cargo")
        .args(["test", "--all"])
        .current_dir(dir)
        .output()
        .await;
    let (test_passed, test_output) = match test_output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            (o.status.success(), format!("{stdout}{stderr}"))
        }
        Err(e) => (false, format!("spawn error: {e}")),
    };

    // Checkout back to main if needed
    if worktree_path.is_none() {
        let _ = tokio::process::Command::new("git")
            .args(["checkout", "main"])
            .output()
            .await;
    }

    CiResult {
        fmt_passed,
        fmt_output,
        clippy_passed,
        clippy_output,
        test_passed,
        test_output,
    }
}

/// Get the diff for a branch compared to main
async fn get_branch_diff(branch: &str) -> String {
    let output = tokio::process::Command::new("git")
        .args(["diff", "main...HEAD", "--stat"])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let diff_stat = String::from_utf8_lossy(&o.stdout).to_string();

            // Also get the actual diff (limited)
            let full_diff = tokio::process::Command::new("git")
                .args(["diff", &format!("main...{branch}")])
                .output()
                .await;

            let full_diff_text = match full_diff {
                Ok(o) => {
                    let text = String::from_utf8_lossy(&o.stdout).to_string();
                    // Limit diff size
                    if text.len() > 10000 {
                        format!("{}...(truncated)", &text[..10000])
                    } else {
                        text
                    }
                }
                Err(_) => String::new(),
            };

            format!("{diff_stat}\n\n{full_diff_text}")
        }
        _ => "Unable to get diff".to_string(),
    }
}

/// Master agent review via Smith
async fn master_review(
    smith: &dyn Smith,
    ingot_id: &str,
    branch: &str,
    diff: &str,
    ci_result: &CiResult,
) -> Result<ReviewResult, SlagError> {
    let prompt = flux::prepare_review_flux(ingot_id, branch, diff, ci_result);

    let response = smith.invoke(&prompt).await?;

    // Parse response
    let approved =
        response.contains("STATUS: APPROVED") || response.to_uppercase().contains("APPROVED");
    let rejected =
        response.contains("STATUS: REJECTED") || response.to_uppercase().contains("REJECTED");

    // Extract comments
    let comments = response
        .lines()
        .skip_while(|l| !l.starts_with("COMMENTS:") && !l.starts_with("Comments:"))
        .skip(1)
        .take(10)
        .collect::<Vec<&str>>()
        .join(" ");

    let approved = if rejected {
        false
    } else {
        approved || ci_result.passed()
    };

    Ok(ReviewResult {
        approved,
        comments: if comments.is_empty() {
            response.lines().take(3).collect::<Vec<&str>>().join(" ")
        } else {
            comments
        },
    })
}

/// Merge a branch back to main
async fn merge_branch(ingot_id: &str) -> Result<(), SlagError> {
    use crate::anvil::worktree;
    worktree::merge_and_cleanup(ingot_id).await
}

/// Clean up a branch without merging
async fn cleanup_branch(ingot_id: &str, keep: bool) {
    if keep {
        println!("    \x1b[90m↳ keeping branch for debugging\x1b[0m");
        return;
    }
    use crate::anvil::worktree;
    worktree::cleanup_without_merge(ingot_id).await;
}

/// Print CI failure details
fn print_ci_failures(ci: &CiResult) {
    if !ci.fmt_passed {
        println!(
            "    \x1b[31m↳ fmt:\x1b[0m {}",
            tui::truncate(&ci.fmt_output, 50)
        );
    }
    if !ci.clippy_passed {
        println!(
            "    \x1b[31m↳ clippy:\x1b[0m {}",
            tui::truncate(&ci.clippy_output, 50)
        );
    }
    if !ci.test_passed {
        println!(
            "    \x1b[31m↳ test:\x1b[0m {}",
            tui::truncate(&ci.test_output, 50)
        );
    }
}

/// List all forge branches
#[allow(dead_code)]
pub async fn list_forge_branches() -> Vec<String> {
    let output = tokio::process::Command::new("git")
        .args(["branch", "--list", "forge/*"])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.lines()
                .map(|l| l.trim().trim_start_matches("* ").to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
        _ => Vec::new(),
    }
}
