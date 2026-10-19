//! The fix report card on a terminal: every escape whole, in both verdicts.

use super::*;

fn outcome(verdict: Verdict) -> Outcome {
    let ok = verdict == Verdict::Fixed;
    Outcome {
        issue: Issue {
            title: "slugify leaves edge dashes: slugify('  Hello, World!  ') returns '-hello-world-'".into(),
            body: String::new(),
            number: Some(12),
            url: None,
            repo: None,
        },
        verdict,
        attempts: 2,
        checks: vec![Run {
            cmd: "python3 -m unittest discover".into(),
            code: Some(if ok { 0 } else { 1 }),
            output: "F.\nAssertionError: '-hello-world-' != 'hello-world'\nFAILED (failures=1)\n".into(),
            secs: 0.4,
            timed_out: false,
        }],
        repro: Repro::Regression { status: "exit 1".into() },
        stat: vec![" pkg/slug.py | 2 +-".into(), " 1 file changed, 1 insertion(+), 1 deletion(-)".into()],
        summary: String::new(),
        branch: ok.then(|| "slag/fix-slugify-leaves-edge-dashes".into()),
        commit: ok.then(|| "0f6ac66".into()),
        pr: None,
        notes: vec!["a note".into()],
        phases: Phases::default(),
        time: Some("wall 4s · api 4s · tools 0s".into()),
        spend: Some("$0.01".into()),
    }
}

#[test]
fn both_verdicts_render_with_every_escape_whole() {
    for verdict in [Verdict::Fixed, Verdict::Failed("the tests still fail after 2 attempts".into())] {
        for width in [79usize, 159] {
            let lines = crate::tui::with_color(true, || report_lines(&outcome(verdict.clone()), width));
            let text = lines.join("\n");
            let problems = crate::tui::ansi_problems(&text);
            assert!(problems.is_empty(), "{problems:?}\n{text}");
            assert!(text.contains('\x1b'), "a terminal render is painted: {text}");
            for line in &lines {
                assert!(crate::tui::visible_len(line) <= width, "{} > {width}: {line:?}", crate::tui::visible_len(line));
            }
        }
    }
}

#[test]
fn a_cut_title_is_shown_whole_from_the_issue_body() {
    let mut out = outcome(Verdict::Fixed);
    let line = "median() returns the wrong value for an even-length list; tests/test_stats.py::test_median_even fails";
    out.issue.title = crate::tui::truncate(line, 60);
    out.issue.body = format!("{line}\n\nmore detail");
    let text = crate::tui::with_color(false, || report_lines(&out, 79)).join("\n");
    assert!(text.contains("test_median_even"), "{text}");
    assert!(text.contains("fails"), "{text}");
    assert!(!text.contains('…'), "{text}");
}

#[test]
fn the_reproduction_reads_as_a_failure_without_the_fix() {
    let text = crate::tui::with_color(false, || report_lines(&outcome(Verdict::Fixed), 120)).join("\n");
    assert!(text.contains("✓ FIXED in 2 attempts"), "{text}");
    // The issue is shown whole, wrapped, never cut with an ellipsis.
    assert!(text.contains("returns '-hello-world-'"), "{text}");
    assert!(!text.contains('…') && !text.contains("..."), "{text}");
    assert!(text.contains("✗ without the fix: exit 1"), "{text}");
    assert!(!text.contains('['), "plain output carries no escape body: {text}");
}

#[test]
fn the_card_drops_instant_phases_and_does_not_repeat_the_review() {
    let mut out = outcome(Verdict::Fixed);
    out.phases = Phases { gather: 0.1, session: 9.6, verify: 0.1, repro: 0.1, review: 0.0, land: 0.1 };
    out.notes = vec!["revised once after review: no regression test was added: add one".into()];
    let text = crate::tui::with_color(false, || report_lines(&out, 79)).join("\n");
    assert!(text.contains("session 9.6s"), "{text}");
    assert!(!text.contains("review 0.0s"), "{text}");
    assert!(text.contains("revised once after review; the revision closed the gap"), "{text}");
    assert!(!text.contains("no regression test was added"), "{text}");
}
