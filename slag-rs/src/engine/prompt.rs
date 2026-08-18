//! prompt — three-tier banded system prompt (hermes pattern).
//!
//! stable (identity + rules, byte-stable across sessions) /
//! context (workspace snapshot, stable within a session) /
//! volatile (date stamp + recipes index, changes daily).
//! Banding keeps the prompt prefix byte-stable so OpenRouter prompt caching holds.

use std::path::Path;
use std::process::Command;

/// Which pass the smith is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    /// Full tool-use pass: edit files, run commands, satisfy the proof.
    Forge,
    /// Read-only survey pass: produce a plan, not edits.
    Plan,
}

/// The three prompt tiers, joined in stable → context → volatile order.
#[derive(Debug, Clone)]
pub struct PromptBands {
    pub stable: String,
    pub context: String,
    pub volatile: String,
}

impl PromptBands {
    pub fn join(&self) -> String {
        format!("{}\n\n{}\n\n{}", self.stable, self.context, self.volatile)
    }
}

/// Build the full banded system prompt for one smith session.
pub fn build(root: &Path, model: &str, mode: PromptMode) -> PromptBands {
    PromptBands {
        stable: stable_band(model, mode),
        context: workspace_snapshot(root, model),
        volatile: volatile_band(root),
    }
}

fn stable_band(model: &str, mode: PromptMode) -> String {
    let mut s = String::new();

    s.push_str("# slag smith\n\n");
    s.push_str(
        "You are a smith at the slag forge. You take one ingot (a scoped task) and work it \
         until the proof rings true. Terse, exact, no ceremony. The metal does not care \
         about your commentary.\n\n",
    );

    match mode {
        PromptMode::Plan => {
            s.push_str("## Mode: survey (read-only)\n\n");
            s.push_str(
                "This is a survey pass. Read and search the workspace, then produce a plan: \
                 ordered steps, files to touch, risks, and how the proof will be satisfied. \
                 Do NOT edit files. Do NOT run mutating commands. Output the plan as your \
                 final text, then call the finish tool with a one-line summary.\n\n",
            );
        }
        PromptMode::Forge => {
            s.push_str("## Mode: forge\n\n");
            s.push_str("Full tool access. Work the ingot until the proof command passes.\n\n");
        }
    }

    s.push_str("## Operating brief\n\n");
    s.push_str("### Gather context first\n");
    s.push_str(
        "- Read and search before editing. Never edit a file you have not read this session.\n\
         - Batch independent reads into one turn; do not read files one at a time when \
           several are needed.\n\
         - Never invent APIs, function names, or file paths. If you have not seen it in \
           this workspace, verify it exists before using it.\n\n",
    );
    s.push_str("### Make changes through tools, not chat\n");
    s.push_str(
        "- All file changes go through the edit and write tools. Never print a code block \
           as a substitute for editing the file.\n\
         - Describing a change is not making it. If the file on disk did not change, \
           nothing happened.\n\n",
    );
    s.push_str("### Verify, then stop\n");
    s.push_str(
        "- Run checks (tests, lint, build, the proof command) before claiming done.\n\
         - After 3 failed attempts on the same file, stop and report what failed, with \
           path:line references and the exact error.\n\
         - Reference code as path:line when reporting.\n\n",
    );
    s.push_str("### No gold-plating\n");
    s.push_str(
        "- Do only what the ingot asks: no extra features, no drive-by refactors, no \
           comments beyond the change.\n\
         - Validate inputs only at system boundaries (CLI args, file reads, network); \
           trust internal callers.\n\
         - Three similar lines beat a premature abstraction.\n\
         - Never remove existing comments unless the ingot asks.\n\n",
    );

    s.push_str("## Rules\n\n");
    s.push_str(
        "- No questions. Decide and proceed; record assumptions in your final summary.\n\
         - Satisfy the :proof command. It is the acceptance gate — exit 0 or the ingot cracks.\n\
         - Minimal diffs. Change only what the ingot requires; no drive-by fixes, no \
           reformatting, no stray comments.\n\
         - Never touch .env files. Never commit unless the ingot explicitly asks.\n\
         - ≤25 words of commentary between tool calls; finish summary ≤120 words.\n\
         - Report outcomes faithfully. Never claim checks pass when output shows failures. \
           Never weaken a test or the proof command to manufacture a pass. Never modify \
           the :proof command unless the ingot explicitly asks.\n\n",
    );

    s.push_str("## Edit style for this model\n\n");
    let m = model.to_lowercase();
    if m.contains("gpt") || m.contains("codex") {
        s.push_str(
            "Prefer larger whole-block replacements: use edit_file to swap a complete \
             function or block in one operation rather than many tiny patches.\n\n",
        );
    } else {
        s.push_str(
            "Prefer surgical replacements: use edit_file with exact old_string/new_string \
             pairs, matching the file byte-for-byte, smallest unique span that pins the edit.\n\n",
        );
    }

    s.push_str(
        "## Finishing\n\n\
         When the work is verified, call the finish tool with a short summary of what \
         changed and how it was verified. If the task specifies an output contract \
         (for example a `CMD: <shell command>` line), that contract line must be the \
         LAST line of your finish summary, alone on its own line, starting at column \
         zero — never inline inside a sentence. Do not keep working after finish.",
    );

    s
}

/// Workspace snapshot: environment identity + git state + detected
/// project facts. Tolerates non-git directories. Stable within a session,
/// so it lives in the cache-safe context band.
pub fn workspace_snapshot(root: &Path, model: &str) -> String {
    let mut s = String::new();
    s.push_str("# Workspace\n(snapshot at session start — re-check with git)\n\n");
    s.push_str(&env_block(model));

    match git(root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Some(branch) => {
            s.push_str(&format!("- branch: {}\n", branch.trim()));
            let dirty = git(root, &["status", "--porcelain"])
                .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            s.push_str(&format!("- dirty files: {dirty}\n"));
            if let Some(log) = git(root, &["log", "--oneline", "-3"]) {
                let log = log.trim();
                if !log.is_empty() {
                    s.push_str("- last commits:\n");
                    for line in log.lines() {
                        // Commit subjects are untrusted text: strip control
                        // chars and cap length before prompt interpolation.
                        let clean: String =
                            line.chars().filter(|c| !c.is_control()).take(100).collect();
                        s.push_str(&format!("  {clean}\n"));
                    }
                }
            }
        }
        None => {
            s.push_str("- no git\n");
        }
    }

    let facts = project_facts(root);
    if !facts.is_empty() {
        s.push_str("\n## Verify commands\n");
        for fact in &facts {
            s.push_str(&format!("- {fact}\n"));
        }
    }

    s
}

/// Environment identity: platform, OS version, shell, and which model is
/// running, plus knowledge cutoffs for the families `openrouter/auto`
/// routes between. All inputs are stable for the life of a session.
fn env_block(model: &str) -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
    format!(
        "<env>\n\
         - platform: {os}/{arch}\n\
         - os version: {ver}\n\
         - shell: {shell}\n\
         </env>\n\
         You are powered by {model}.\n\n\
         ## Knowledge cutoffs (routed families)\n\
         - openai/gpt-5*: ~Oct 2024\n\
         - anthropic/claude*: ~Mar 2025\n\
         - google/gemini-2.5*: ~Jan 2025\n\
         - qwen/qwen3*: ~Apr 2025\n\
         - deepseek*: ~Jul 2024\n\
         Anything newer than your cutoff: verify in the workspace, do not guess.\n\n",
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        ver = os_version(),
    )
}

/// Kernel release via `uname -r`; "unknown" wherever that fails
/// (e.g. Windows). Good enough for prompt identity, zero new deps.
fn os_version() -> String {
    Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

const VERIFY_NAMES: [&str; 5] = ["test", "lint", "typecheck", "build", "check"];

/// Detect verify commands from project manifests. Capped at 8 facts.
fn project_facts(root: &Path) -> Vec<String> {
    let mut facts = Vec::new();

    if let Ok(raw) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) {
                for (name, _) in scripts {
                    if VERIFY_NAMES.iter().any(|v| name.contains(v)) {
                        facts.push(format!("npm run {name}"));
                    }
                }
            }
        }
    }

    if let Ok(raw) = std::fs::read_to_string(root.join("Makefile")) {
        for line in raw.lines() {
            if let Some((target, _)) = line.split_once(':') {
                let target = target.trim();
                if !target.is_empty()
                    && !target.contains(char::is_whitespace)
                    && !target.starts_with('.')
                    && !target.starts_with('#')
                    && VERIFY_NAMES.iter().any(|v| target.contains(v))
                {
                    facts.push(format!("make {target}"));
                }
            }
        }
    }

    if root.join("Cargo.toml").exists() {
        facts.push("cargo test".to_string());
    }

    if root.join("pytest.ini").exists() || root.join("pyproject.toml").exists() {
        facts.push("pytest".to_string());
    }

    facts.truncate(8);
    facts
}

fn volatile_band(root: &Path) -> String {
    let date = chrono::Local::now().format("%Y-%m-%d");
    let tool_names: Vec<String> = super::tools::ToolBox::specs()
        .into_iter()
        .map(|s| s.name)
        .collect();
    let index = super::tools::recipes::index(root, &tool_names);
    format!(
        "# Session\n- date: {date}\n\n{index}\n\nIf a recipe matches the task, load it with \
         recipe_view before working. Err on the side of loading."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_join_in_order() {
        let bands = PromptBands {
            stable: "STABLE".into(),
            context: "CONTEXT".into(),
            volatile: "VOLATILE".into(),
        };
        let joined = bands.join();
        let s = joined.find("STABLE").unwrap();
        let c = joined.find("CONTEXT").unwrap();
        let v = joined.find("VOLATILE").unwrap();
        assert!(s < c && c < v);
    }

    #[test]
    fn volatile_date_has_no_clock_time() {
        let dir = tempfile::tempdir().unwrap();
        let v = volatile_band(dir.path());
        let date_line = v.lines().find(|l| l.starts_with("- date:")).unwrap();
        let stamp = date_line.trim_start_matches("- date:").trim();
        assert_eq!(stamp.len(), 10, "expected YYYY-MM-DD, got {stamp}");
        assert!(!stamp.contains(':'), "date stamp must not carry clock time");
        assert!(chrono::NaiveDate::parse_from_str(stamp, "%Y-%m-%d").is_ok());
    }

    #[test]
    fn gpt_model_gets_whole_block_guidance() {
        let s = stable_band("openai/gpt-5", PromptMode::Forge);
        assert!(s.contains("whole-block"));
        assert!(!s.contains("old_string/new_string"));
        let c = stable_band("openai/codex-mini", PromptMode::Forge);
        assert!(c.contains("whole-block"));
    }

    #[test]
    fn qwen_model_gets_surgical_replace_guidance() {
        let s = stable_band("qwen/qwen3-coder", PromptMode::Forge);
        assert!(s.contains("old_string/new_string"));
        assert!(!s.contains("whole-block"));
    }

    #[test]
    fn plan_mode_instructs_plan_not_edits() {
        let s = stable_band("qwen/qwen3-coder", PromptMode::Plan);
        assert!(s.contains("survey"));
        assert!(s.contains("Do NOT edit files"));
        let f = stable_band("qwen/qwen3-coder", PromptMode::Forge);
        assert!(!f.contains("Do NOT edit files"));
    }

    #[test]
    fn stable_band_carries_numeric_length_anchors() {
        for mode in [PromptMode::Forge, PromptMode::Plan] {
            let s = stable_band("qwen/qwen3-coder", mode);
            assert!(s.contains("≤25 words of commentary between tool calls"), "{s}");
            assert!(s.contains("finish summary ≤120 words"), "{s}");
        }
    }

    #[test]
    fn stable_band_carries_faithful_reporting_rule() {
        let s = stable_band("qwen/qwen3-coder", PromptMode::Forge);
        let rules = &s[s.find("## Rules").unwrap()..];
        assert!(rules.contains("Report outcomes faithfully."));
        assert!(rules.contains("Never claim checks pass when output shows failures."));
        assert!(rules.contains("Never weaken a test or the proof command to manufacture a pass."));
        assert!(rules.contains("Never modify the :proof command unless the ingot explicitly asks."));
    }

    #[test]
    fn stable_band_carries_anti_gold_plating_rules() {
        for mode in [PromptMode::Forge, PromptMode::Plan] {
            let s = stable_band("qwen/qwen3-coder", mode);
            let section = &s[s.find("### No gold-plating").unwrap()..];
            assert!(section.contains("no extra features, no drive-by refactors"));
            assert!(section.contains("Validate inputs only at system boundaries"));
            assert!(section.contains("trust internal callers"));
            assert!(section.contains("Three similar lines beat a premature abstraction."));
            assert!(section.contains("Never remove existing comments unless the ingot asks."));
        }
    }

    #[test]
    fn env_block_carries_identity_and_cutoffs() {
        let e = env_block("qwen/qwen3-coder");
        assert!(e.contains("<env>") && e.contains("</env>"));
        assert!(e.contains(&format!(
            "- platform: {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )));
        assert!(e.contains("- os version: "));
        assert!(e.contains("- shell: "));
        assert!(e.contains("You are powered by qwen/qwen3-coder."));
        let cutoffs = &e[e.find("## Knowledge cutoffs (routed families)").unwrap()..];
        for family in ["openai/gpt-5*", "anthropic/claude*", "qwen/qwen3*", "deepseek*"] {
            assert!(cutoffs.contains(family), "missing cutoff row for {family}");
        }
        assert!(cutoffs.contains("verify in the workspace, do not guess"));
    }

    #[test]
    fn snapshot_embeds_env_block_before_git_state() {
        let dir = tempfile::tempdir().unwrap();
        let snap = workspace_snapshot(dir.path(), "openai/gpt-5");
        let env = snap.find("<env>").unwrap();
        let git = snap.find("no git").unwrap();
        assert!(env < git, "env block must precede git state");
        assert!(snap.contains("You are powered by openai/gpt-5."));
    }

    #[test]
    fn os_version_is_single_trimmed_line() {
        let v = os_version();
        assert!(!v.is_empty());
        assert!(!v.contains('\n'));
        assert_eq!(v, v.trim());
    }

    #[test]
    fn stable_band_is_byte_stable_across_runs() {
        let a = stable_band("openai/gpt-5", PromptMode::Forge);
        let b = stable_band("openai/gpt-5", PromptMode::Forge);
        assert_eq!(a, b, "stable band must be byte-identical across builds");
    }

    #[test]
    fn snapshot_tolerates_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let snap = workspace_snapshot(dir.path(), "qwen/qwen3-coder");
        assert!(snap.contains("no git"));
        assert!(snap.contains("snapshot at session start — re-check with git"));
    }

    #[test]
    fn snapshot_sanitizes_commit_subjects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "forge@slag.test"]);
        run(&["config", "user.name", "slag"]);
        let subject = format!("evil\x1b[31m {}", "x".repeat(200));
        run(&["commit", "--allow-empty", "-m", &subject]);

        let snap = workspace_snapshot(root, "qwen/qwen3-coder");
        assert!(!snap.contains('\x1b'), "ESC must not reach the prompt");
        let line = snap.lines().find(|l| l.contains("evil")).unwrap();
        // "  " indent + subject line capped at 100 chars.
        assert!(line.chars().count() <= 102, "got {} chars", line.chars().count());
    }

    #[test]
    fn detects_verify_commands_from_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"x","scripts":{"test":"vitest","lint":"eslint .","dev":"vite","typecheck":"tsc --noEmit"}}"#,
        )
        .unwrap();
        let facts = project_facts(dir.path());
        assert!(facts.contains(&"npm run test".to_string()));
        assert!(facts.contains(&"npm run lint".to_string()));
        assert!(facts.contains(&"npm run typecheck".to_string()));
        assert!(!facts.iter().any(|f| f.contains("dev")));
    }

    #[test]
    fn facts_capped_at_eight() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"a","test:unit":"a","test:e2e":"a","lint":"a","lint:fix":"a","typecheck":"a","build":"a","check":"a","check:all":"a"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let facts = project_facts(dir.path());
        assert_eq!(facts.len(), 8);
    }

    #[test]
    fn detects_makefile_and_cargo_and_pytest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "test:\n\tcargo test\nlint:\n\tclippy\ndeploy:\n\tship\n.PHONY: test lint\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.pytest]\n").unwrap();
        let facts = project_facts(dir.path());
        assert!(facts.contains(&"make test".to_string()));
        assert!(facts.contains(&"make lint".to_string()));
        assert!(!facts.iter().any(|f| f == "make deploy"));
        assert!(facts.contains(&"cargo test".to_string()));
        assert!(facts.contains(&"pytest".to_string()));
    }

    #[test]
    fn build_produces_all_three_bands() {
        let dir = tempfile::tempdir().unwrap();
        let bands = build(dir.path(), "qwen/qwen3-coder", PromptMode::Forge);
        assert!(bands.stable.contains("slag smith"));
        assert!(bands.context.contains("no git"));
        assert!(bands.volatile.contains("## Recipes"));
        assert!(bands.join().contains("(none installed)"));
    }

    #[test]
    fn volatile_band_carries_load_biasing_preamble() {
        let dir = tempfile::tempdir().unwrap();
        let v = volatile_band(dir.path());
        assert!(v.contains("## Recipes"));
        assert!(v.contains("load it with recipe_view"), "{v}");
        assert!(v.contains("Err on the side of loading"), "{v}");
    }
}
