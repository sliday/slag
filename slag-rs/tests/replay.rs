//! `slag replay` against the recorded fixture runs: it plays without a key
//! or a network, in both renderers, and the same tape draws the same frame.

use assert_cmd::Command;

const FORGE: &str = "tests/fixtures/runs/textstats.tape.jsonl";
const TEMPER: &str = "tests/fixtures/runs/hello-goal.tape.jsonl";

fn fixture(rel: &str) -> String {
    format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"))
}

/// A replay in an empty directory with no key anywhere: nothing it needs
/// may come from the network or the user's setup.
fn slag(scratch: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("slag").unwrap();
    cmd.current_dir(scratch.path())
        .env_remove("OPENROUTER_API_KEY")
        .env("SLAG_CONFIG_DIR", scratch.path())
        .env("NO_COLOR", "1");
    cmd
}

fn stdout(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_stream_view_plays_a_recorded_forge_to_its_assay() {
    let scratch = tempfile::tempdir().unwrap();
    let out = stdout(slag(&scratch).args(["replay", &fixture(FORGE), "--at", "assay", "--size", "100x30"]));
    for needle in ["SURVEYOR", "FORGE", "ASSAY", "CRACKED", "anvils", "heat 2/5"] {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }
    // Every line fits the pinned width: nothing wraps in the ledger.
    for line in out.lines() {
        assert!(line.chars().count() <= 100, "{} > 100: {line}", line.chars().count());
    }
    // A replay leaves the directory it ran in untouched.
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[test]
fn the_dashboard_frame_at_a_moment_is_the_same_every_time() {
    let scratch = tempfile::tempdir().unwrap();
    let args = ["replay", &fixture(FORGE), "--tui", "--at", "retry", "--size", "120x36"];
    let first = stdout(slag(&scratch).args(args));
    let second = stdout(slag(&scratch).args(args));
    assert_eq!(first, second, "same tape, same point, same frame");
    assert_eq!(first.lines().count(), 36, "{first}");
    assert!(first.contains("crucible"), "{first}");
    assert!(first.contains("forging"), "{first}");
}

#[test]
fn the_warden_verdict_reaches_both_views() {
    let scratch = tempfile::tempdir().unwrap();
    let stream = stdout(slag(&scratch).args(["replay", &fixture(TEMPER), "--at", "assay", "--size", "100x30"]));
    assert!(stream.contains("WARDEN"), "{stream}");
    let dash = stdout(slag(&scratch).args(["replay", &fixture(TEMPER), "--tui", "--at", "warden", "--size", "100x30"]));
    assert!(dash.contains("warden"), "{dash}");
}

#[test]
fn points_lists_the_moments_a_tape_has() {
    let scratch = tempfile::tempdir().unwrap();
    let out = stdout(slag(&scratch).args(["replay", &fixture(FORGE), "--points"]));
    for name in ["start", "plan", "anvils", "retry", "crack", "assay"] {
        assert!(out.contains(name), "{out}");
    }
}

#[test]
fn a_missing_point_is_an_error_that_names_the_choices() {
    let scratch = tempfile::tempdir().unwrap();
    let out = slag(&scratch)
        .args(["replay", &fixture(FORGE), "--at", "nowhere"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("retry"), "{err}");
}
