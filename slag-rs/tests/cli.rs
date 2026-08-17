//! End-to-end CLI behavior for the no-key path.
//!
//! slag has exactly one prerequisite: an OpenRouter key. These tests fix
//! what a machine without one sees. Each runs in a throwaway directory
//! with a throwaway `SLAG_CONFIG_DIR` and every slag env var stripped, so
//! a developer's real key in the shell or in `~/.config/slag` can never
//! make them pass by accident.

use std::time::Duration;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

/// Every env var that could smuggle a key or a model into a run.
const SLAG_VARS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "SLAG_MODEL_BASE",
    "SLAG_MODEL_PLAN",
    "SLAG_MODEL_ALT",
    "SLAG_MODEL_JUDGE",
    "SLAG_DUEL",
    "SLAG_DUEL_ROUNDS",
    "SLAG_SCREENSHOT_CMD",
    "SLAG_REASONING_EFFORT",
    "SLAG_OPENROUTER_BASE",
    "SLAG_CONFIG_DIR",
];

/// A slag with no key anywhere: empty project dir, empty config dir.
/// stdin is a closed pipe (assert_cmd never hands the child a terminal),
/// which is the headless case these tests care about.
fn slag(project: &TempDir, config: &TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slag"));
    for var in SLAG_VARS {
        cmd.env_remove(var);
    }
    cmd.env("SLAG_CONFIG_DIR", config.path())
        .current_dir(project.path())
        // A prompt that blocks on stdin would hang CI forever; fail instead.
        .timeout(Duration::from_secs(30));
    cmd
}

fn dirs() -> (TempDir, TempDir) {
    (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
}

/// `status` reports on a forge; it must never demand the key the user
/// came here to diagnose.
#[test]
fn status_works_without_any_key() {
    let (project, config) = dirs();
    slag(&project, &config)
        .arg("status")
        .assert()
        .success()
        .stdout(contains("No crucible found"));
}

/// The headless first run: no key, no terminal to type one into. The
/// error has to name the variable to export and the site to get a key
/// from, and it has to arrive instead of a blocked stdin read.
#[test]
fn forge_without_a_key_fails_fast_with_an_actionable_message() {
    let (project, config) = dirs();
    let assert = slag(&project, &config)
        .arg("build me a website")
        .assert()
        .failure()
        .stderr(contains("OPENROUTER_API_KEY"))
        .stderr(contains("openrouter.ai"));

    assert_eq!(
        assert.get_output().status.code(),
        Some(1),
        "a missing key is a setup failure, not a crash"
    );
}

/// Bare `slag key` with nothing configured and no terminal: report the
/// setup rather than prompting into a pipe. Nothing to verify means no
/// network call, so the panel comes back immediately.
#[test]
fn bare_key_prints_the_status_panel_without_a_key() {
    let (project, config) = dirs();
    slag(&project, &config)
        // Any network call would go here; nothing may reach it.
        .env("SLAG_OPENROUTER_BASE", "http://127.0.0.1:1/v1")
        .arg("key")
        .assert()
        .success()
        .stdout(contains("none"))
        .stdout(contains("openrouter/auto"))
        .stdout(contains("OPENROUTER_API_KEY"));
}

/// The key subcommand is the whole configuration surface, so it has to be
/// discoverable from `--help`.
#[test]
fn help_lists_the_key_subcommand() {
    let (project, config) = dirs();
    slag(&project, &config)
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("key"))
        .stdout(contains("OpenRouter key"));
}
