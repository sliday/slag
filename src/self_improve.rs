use std::path::{Path, PathBuf};

use crate::config::{PipelineConfig, SmithConfig};
use crate::error::SlagError;
use crate::tui;

const UPSTREAM_REPO: &str = "https://github.com/sliday/slag.git";
const GH_REPO: &str = "sliday/slag";
const SLAG_ARTIFACTS: &[&str] = &[
    "PLAN.md",
    "BLUEPRINT.md",
    "PRD.md",
    "PROGRESS.md",
    "AGENTS.md",
    "PHASES.md",
];

/// Metrics measured before and after self-improvement.
#[derive(Debug, Clone)]
struct Metrics {
    test_pass: usize,
    test_fail: usize,
    clippy_warnings: usize,
}

impl Metrics {
    async fn measure(dir: &Path) -> Self {
        let (test_pass, test_fail) = parse_test_output(dir).await;
        let clippy_warnings = count_clippy_warnings(dir).await;
        Self {
            test_pass,
            test_fail,
            clippy_warnings,
        }
    }

    fn is_improvement_over(&self, baseline: &Self) -> bool {
        if self.test_fail > baseline.test_fail {
            return false;
        }
        let more_tests = self.test_pass > baseline.test_pass;
        let fewer_warnings = self.clippy_warnings < baseline.clippy_warnings;
        let fewer_failures = self.test_fail < baseline.test_fail;
        more_tests || fewer_warnings || fewer_failures
    }

    fn summary(&self) -> String {
        format!(
            "tests: {} pass / {} fail, clippy: {} warnings",
            self.test_pass, self.test_fail, self.clippy_warnings
        )
    }

    fn delta_summary(&self, baseline: &Self) -> String {
        let test_delta = self.test_pass as i64 - baseline.test_pass as i64;
        let clippy_delta = self.clippy_warnings as i64 - baseline.clippy_warnings as i64;
        format!("tests: {:+}, clippy: {:+}", test_delta, clippy_delta)
    }
}

/// Run the self-improvement loop:
/// 1. Clone slag from GitHub into a sandbox
/// 2. Measure baseline
/// 3. Forge the improvement
/// 4. Measure after
/// 5. If improved, create PR via `gh`
pub async fn run(
    target: &str,
    smith_config: &SmithConfig,
    pipeline_config: &PipelineConfig,
) -> Result<(), SlagError> {
    tui::header("SELF-IMPROVE \u{00b7} meta-forge");

    // Pre-flight: gh CLI available
    if !has_gh_cli().await {
        return Err(SlagError::WorktreeError(
            "gh CLI not found — install from https://cli.github.com to create PRs".into(),
        ));
    }

    // Check for existing self-improve PRs
    let existing_prs = list_existing_prs().await;
    let continue_branch = if !existing_prs.is_empty() {
        tui::status_line("0", tui::COLD, "Existing self-improve PRs found:");
        for (i, pr) in existing_prs.iter().enumerate() {
            println!("  \x1b[90m{}. {} — {}\x1b[0m", i + 1, pr.number, pr.title);
        }
        print!(
            "  \x1b[38;5;220m?\x1b[0m [N]ew PR / [1-{}] continue existing / [Q]uit: ",
            existing_prs.len()
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let choice = crate::prompt::read_line_timeout(30)
            .unwrap_or_else(|| "n".to_string());
        let choice = choice.trim().to_lowercase();
        if choice == "q" || choice == "quit" {
            return Ok(());
        }
        if let Ok(idx) = choice.parse::<usize>() {
            if idx >= 1 && idx <= existing_prs.len() {
                Some(existing_prs[idx - 1].branch.clone())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Clone slag from GitHub
    tui::status_line("1", tui::COLD, "Cloning slag from GitHub...");
    let sandbox = clone_upstream().await?;
    println!("  \x1b[90msandbox: {}\x1b[0m", sandbox.display());

    // If continuing an existing PR, checkout that branch
    if let Some(ref branch) = continue_branch {
        tui::status_line("1b", tui::COLD, &format!("Checking out {branch}..."));
        git_cmd(&sandbox, &["fetch", "origin", branch]).await;
        git_cmd(&sandbox, &["checkout", "-b", branch, &format!("origin/{branch}")]).await;
    }

    // Measure baseline
    tui::status_line("2", tui::COLD, "Measuring baseline...");
    let baseline = Metrics::measure(&sandbox).await;
    if baseline.test_fail > 0 {
        cleanup_sandbox(&sandbox).await;
        return Err(SlagError::WorktreeError(format!(
            "upstream has {} failing tests — cannot self-improve a broken baseline",
            baseline.test_fail
        )));
    }
    println!("  \x1b[90mbaseline: {}\x1b[0m", baseline.summary());

    // Generate commission
    let commission = generate_commission(target, &baseline);
    let prd_path = sandbox.join("PRD.md");
    std::fs::write(&prd_path, &commission)?;

    let display_target = if target.len() > 60 {
        format!("{}...", &target[..57])
    } else {
        target.to_string()
    };
    tui::status_line("3", tui::WARM, &format!("Commission: {display_target}"));

    // Create or reuse branch for the PR
    let branch_name = if let Some(ref branch) = continue_branch {
        branch.clone()
    } else {
        let name = make_branch_name(target);
        git_cmd(&sandbox, &["checkout", "-b", &name]).await;
        name
    };

    // Run pipeline in sandbox
    tui::status_line("4", tui::HOT, "Forging in sandbox...");
    let original_dir = std::env::current_dir()?;
    std::env::set_current_dir(&sandbox)?;
    let _ = std::fs::create_dir_all("logs");

    let forge_result =
        crate::pipeline::run(Some(&commission), smith_config, pipeline_config).await;

    std::env::set_current_dir(&original_dir)?;

    if let Err(ref e) = forge_result {
        tui::status_line("x", tui::WARM, &format!("Forge failed: {e}"));
        cleanup_sandbox(&sandbox).await;
        return forge_result;
    }

    // Measure after
    tui::status_line("5", tui::BRIGHT, "Measuring improvement...");
    let after = Metrics::measure(&sandbox).await;
    println!("  \x1b[90mafter: {}\x1b[0m", after.summary());
    println!(
        "  \x1b[90mdelta: {}\x1b[0m",
        after.delta_summary(&baseline)
    );

    if !after.is_improvement_over(&baseline) {
        tui::status_line("6", tui::WARM, "No measurable improvement — discarding");
        cleanup_sandbox(&sandbox).await;
        return Ok(());
    }

    // Strip slag artifacts, commit source changes
    tui::status_line("6", tui::PURE, "Improvement detected — preparing PR");
    strip_artifacts(&sandbox).await;
    git_cmd(
        &sandbox,
        &[
            "commit",
            "-a",
            "--allow-empty",
            "-m",
            &format!("self-improve: {target}"),
            "--quiet",
        ],
    )
    .await;

    // Push and create/update PR
    let pr_url = if continue_branch.is_some() {
        tui::status_line("7", tui::PURE, "Pushing to existing PR...");
        git_cmd(&sandbox, &["push", "origin", &branch_name]).await;
        format!("(pushed to existing branch {branch_name})")
    } else {
        tui::status_line("7", tui::PURE, "Creating pull request...");
        create_pr(&sandbox, &branch_name, target, &baseline, &after).await?
    };

    cleanup_sandbox(&sandbox).await;

    tui::status_line(
        ">>",
        tui::PURE,
        &format!(
            "PR created ({}) — {}",
            after.delta_summary(&baseline),
            pr_url
        ),
    );

    Ok(())
}

fn generate_commission(target: &str, baseline: &Metrics) -> String {
    let preamble = format!(
        "This is the slag task orchestrator (Rust). Current state: {} tests pass, {} clippy warnings.\n\
        Acceptance: cargo test --all passes with 0 failures.\n\n",
        baseline.test_pass, baseline.clippy_warnings
    );

    match target {
        "quality" => format!(
            "{preamble}Improve code quality. Fix clippy warnings. \
            Do NOT modify test files unless adding new tests."
        ),
        "tests" => format!(
            "{preamble}Add unit tests for untested functions. \
            Target: increase test count by at least 5."
        ),
        "performance" => format!(
            "{preamble}Optimize performance — reduce unnecessary allocations, \
            avoid redundant file reads, minimize string copies in hot paths."
        ),
        "tokens" => format!(
            "{preamble}Reduce LLM token usage in prompts (src/flux.rs). \
            Shorten templates, compress repeated data. \
            Do NOT change prompt semantics."
        ),
        // Freeform commission — user's exact words
        freeform => format!("{preamble}{freeform}"),
    }
}

fn make_branch_name(target: &str) -> String {
    let slug: String = target
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let slug = slug.trim_end_matches('-');
    let ts = chrono::Utc::now().timestamp();
    format!("self-improve/{slug}-{ts}")
}

// --- PR discovery ---

struct ExistingPr {
    number: String,
    title: String,
    branch: String,
}

async fn list_existing_prs() -> Vec<ExistingPr> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--repo",
            GH_REPO,
            "--state",
            "open",
            "--search",
            "self-improve in:title",
            "--json",
            "number,title,headRefName",
        ])
        .output()
        .await;

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // Parse JSON array: [{"number":142,"title":"...","headRefName":"..."},...]
    let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            Some(ExistingPr {
                number: format!("#{}", item.get("number")?.as_u64()?),
                title: item.get("title")?.as_str()?.to_string(),
                branch: item.get("headRefName")?.as_str()?.to_string(),
            })
        })
        .collect()
}

// --- Sandbox lifecycle ---

async fn clone_upstream() -> Result<PathBuf, SlagError> {
    let ts = chrono::Utc::now().timestamp();
    let sandbox = PathBuf::from(format!("/tmp/slag-self-improve-{ts}"));

    let output = tokio::process::Command::new("git")
        .args(["clone", "--depth", "1", UPSTREAM_REPO, sandbox.to_str().unwrap()])
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("git clone failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlagError::WorktreeError(format!(
            "git clone failed: {stderr}"
        )));
    }

    Ok(sandbox)
}

async fn strip_artifacts(sandbox: &Path) {
    for artifact in SLAG_ARTIFACTS {
        let path = sandbox.join(artifact);
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::remove_dir_all(sandbox.join("logs"));
    git_cmd(sandbox, &["add", "-A"]).await;
}

async fn create_pr(
    sandbox: &Path,
    branch: &str,
    target: &str,
    baseline: &Metrics,
    after: &Metrics,
) -> Result<String, SlagError> {
    // Push branch to origin (user's fork or upstream if they have access)
    let push_output = tokio::process::Command::new("git")
        .args(["push", "-u", "origin", branch])
        .current_dir(sandbox)
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("git push failed: {e}")))?;

    if !push_output.status.success() {
        let stderr = String::from_utf8_lossy(&push_output.stderr);
        return Err(SlagError::WorktreeError(format!(
            "git push failed (do you have write access or a fork?): {stderr}"
        )));
    }

    let title = format!("self-improve: {}", truncate(target, 60));
    let body = format!(
        "## Self-Improvement\n\n\
        Generated by `slag self-improve`.\n\n\
        **Commission:** {target}\n\n\
        ## Metrics\n\n\
        | Metric | Before | After | Delta |\n\
        |--------|--------|-------|-------|\n\
        | Tests pass | {} | {} | {:+} |\n\
        | Tests fail | {} | {} | {:+} |\n\
        | Clippy warnings | {} | {} | {:+} |\n\n\
        ## Verification\n\n\
        - [ ] `cargo test --all` passes\n\
        - [ ] `cargo clippy -- -D warnings` clean\n\
        - [ ] Changes are minimal and targeted\n",
        baseline.test_pass,
        after.test_pass,
        after.test_pass as i64 - baseline.test_pass as i64,
        baseline.test_fail,
        after.test_fail,
        after.test_fail as i64 - baseline.test_fail as i64,
        baseline.clippy_warnings,
        after.clippy_warnings,
        after.clippy_warnings as i64 - baseline.clippy_warnings as i64,
    );

    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "create",
            "--repo",
            GH_REPO,
            "--title",
            &title,
            "--body",
            &body,
            "--head",
            branch,
        ])
        .current_dir(sandbox)
        .output()
        .await
        .map_err(|e| SlagError::WorktreeError(format!("gh pr create failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlagError::WorktreeError(format!(
            "gh pr create failed: {stderr}"
        )));
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(url)
}

async fn cleanup_sandbox(sandbox: &Path) {
    let _ = std::fs::remove_dir_all(sandbox);
}

// --- Utilities ---

async fn git_cmd(dir: &Path, args: &[&str]) {
    let _ = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await;
}

async fn has_gh_cli() -> bool {
    tokio::process::Command::new("gh")
        .args(["--version"])
        .output()
        .await
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn parse_test_output(dir: &Path) -> (usize, usize) {
    let output = tokio::process::Command::new("cargo")
        .args(["test", "--all"])
        .current_dir(dir)
        .output()
        .await;

    let Ok(output) = output else {
        return (0, 0);
    };

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut pass = 0usize;
    let mut fail = 0usize;
    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("passed") {
            if let Some(n) = extract_number_before(&lower, "passed") {
                pass = n;
            }
        }
        if lower.contains("failed") && !lower.contains("0 failed") {
            if let Some(n) = extract_number_before(&lower, "failed") {
                fail = n;
            }
        }
    }
    (pass, fail)
}

async fn count_clippy_warnings(dir: &Path) -> usize {
    let output = tokio::process::Command::new("cargo")
        .args(["clippy", "--all", "--", "-W", "clippy::all"])
        .current_dir(dir)
        .output()
        .await;

    let Ok(output) = output else {
        return 0;
    };

    let text = String::from_utf8_lossy(&output.stderr);
    text.lines()
        .filter(|l| l.trim_start().starts_with("warning:") || l.trim_start().starts_with("error:"))
        .count()
}

fn extract_number_before(text: &str, keyword: &str) -> Option<usize> {
    let idx = text.find(keyword)?;
    let before = &text[..idx];
    before
        .split_whitespace()
        .next_back()?
        .trim_end_matches(';')
        .parse()
        .ok()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_improvement_detection() {
        let baseline = Metrics {
            test_pass: 100,
            test_fail: 0,
            clippy_warnings: 5,
        };
        let improved = Metrics {
            test_pass: 105,
            test_fail: 0,
            clippy_warnings: 3,
        };
        let regressed = Metrics {
            test_pass: 100,
            test_fail: 2,
            clippy_warnings: 3,
        };
        let unchanged = Metrics {
            test_pass: 100,
            test_fail: 0,
            clippy_warnings: 5,
        };

        assert!(improved.is_improvement_over(&baseline));
        assert!(!regressed.is_improvement_over(&baseline));
        assert!(!unchanged.is_improvement_over(&baseline));
    }

    #[test]
    fn extract_number_from_test_output() {
        assert_eq!(
            extract_number_before("134 passed; 0 failed", "passed"),
            Some(134)
        );
        assert_eq!(
            extract_number_before("test result: ok. 42 passed", "passed"),
            Some(42)
        );
        assert_eq!(extract_number_before("2 failed", "failed"), Some(2));
    }

    #[test]
    fn extract_number_before_empty_string_returns_none() {
        assert_eq!(extract_number_before("", "passed"), None);
    }

    #[test]
    fn commission_shortcuts_include_baseline() {
        let baseline = Metrics {
            test_pass: 134,
            test_fail: 0,
            clippy_warnings: 3,
        };
        let commission = generate_commission("quality", &baseline);
        assert!(commission.contains("134"));
        assert!(commission.contains("3 clippy"));
    }

    #[test]
    fn commission_freeform_passes_through() {
        let baseline = Metrics {
            test_pass: 100,
            test_fail: 0,
            clippy_warnings: 0,
        };
        let commission =
            generate_commission("Add streaming support to smith invocation", &baseline);
        assert!(commission.contains("Add streaming support to smith invocation"));
        assert!(commission.contains("100 tests pass")); // preamble included
    }

    #[test]
    fn branch_name_slugifies() {
        let name = make_branch_name("Fix error handling in forge loop");
        assert!(name.starts_with("self-improve/fix-error-handling"));
        assert!(name.len() <= 80);
    }

    #[test]
    fn delta_summary_format() {
        let baseline = Metrics {
            test_pass: 100,
            test_fail: 0,
            clippy_warnings: 5,
        };
        let after = Metrics {
            test_pass: 105,
            test_fail: 0,
            clippy_warnings: 2,
        };
        assert_eq!(after.delta_summary(&baseline), "tests: +5, clippy: -3");
    }
}
