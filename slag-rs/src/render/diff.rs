//! Word-level diff rendering with a change-ratio fallback.
//!
//! An `edit_file` result is a before/after pair, and slag used to show
//! only a raw preview of the after side: you could see that something
//! changed without seeing *what*. A line diff is better but still coarse
//! — a one-word rename lights the whole line red-then-green and the eye
//! has to find the word itself.
//!
//! So: diff by line first, then re-diff each replaced pair by word and
//! highlight only the spans that moved. With one guard. Past
//! `CHANGE_THRESHOLD` of the line actually changing, a word diff stops
//! helping and starts producing confetti — alternating fragments that
//! read as noise. Above the threshold the pair falls back to plain
//! full-line coloring, which is what a rewritten line honestly is.
//!
//! Output is a `Vec<DiffLine>` of `(kind, spans)`. Rendering it is the
//! caller's job: the dashboard maps spans onto ratatui styles, stream
//! mode maps them onto ANSI. Keeping this module free of both makes the
//! interesting part — where the threshold bites — testable as data.

use similar::{ChangeTag, TextDiff};

/// Past this fraction of a line changing, word granularity stops helping.
/// Claude Code's StructuredDiff uses the same 0.4.
pub const CHANGE_THRESHOLD: f64 = 0.4;

/// What a rendered line represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Present in both sides.
    Context,
    /// Only on the old side.
    Removed,
    /// Only on the new side.
    Added,
}

/// How a run of text inside a line changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// Unchanged within an otherwise-changed line.
    Same,
    /// The part that actually moved — the bit worth looking at.
    Changed,
}

/// A run of text with its own emphasis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSpan {
    pub kind: SpanKind,
    pub text: String,
}

/// One rendered line: a gutter marker plus the spans that make it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub spans: Vec<DiffSpan>,
}

impl DiffLine {
    fn plain(kind: LineKind, text: &str) -> Self {
        DiffLine { kind, spans: vec![DiffSpan { kind: SpanKind::Same, text: text.to_string() }] }
    }

    /// The gutter marker: `-`, `+`, or a space.
    pub fn marker(&self) -> char {
        match self.kind {
            LineKind::Context => ' ',
            LineKind::Removed => '-',
            LineKind::Added => '+',
        }
    }

    /// The line's text with the spans concatenated — what a plain-text
    /// consumer (a log, an assertion) wants.
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// True when any span was marked as changed.
    #[cfg(test)]
    pub fn has_intraline_highlight(&self) -> bool {
        self.spans.iter().any(|s| s.kind == SpanKind::Changed)
    }
}

/// Fraction of `old`→`new` that is not shared, by word. 0.0 for identical
/// strings, 1.0 when nothing survives. Two empty strings are identical,
/// not maximally changed.
///
/// Measured against both sides together — `(deleted + inserted) /
/// (len(old) + len(new))`. A replacement contributes a deletion *and* an
/// insertion, so scoring it against one side's length counts the same
/// edit twice and pushes ordinary one-word renames over the threshold.
pub fn change_ratio(old: &str, new: &str) -> f64 {
    let diff = TextDiff::from_words(old, new);
    let (mut same, mut moved) = (0usize, 0usize);
    for change in diff.iter_all_changes() {
        let n = change.value().chars().count();
        match change.tag() {
            // Shared text lives on both sides, so it counts on both.
            ChangeTag::Equal => same += 2 * n,
            _ => moved += n,
        }
    }
    let total = same + moved;
    if total == 0 {
        return 0.0;
    }
    moved as f64 / total as f64
}

/// Split one replaced line pair into word-level spans. Returns the
/// removed line and the added line, each carrying `Changed` spans only
/// where the words differ.
fn word_spans(old: &str, new: &str) -> (DiffLine, DiffLine) {
    let diff = TextDiff::from_words(old, new);
    let (mut rem, mut add): (Vec<DiffSpan>, Vec<DiffSpan>) = (Vec::new(), Vec::new());
    for change in diff.iter_all_changes() {
        let text = change.value().to_string();
        match change.tag() {
            ChangeTag::Equal => {
                push_span(&mut rem, SpanKind::Same, &text);
                push_span(&mut add, SpanKind::Same, &text);
            }
            ChangeTag::Delete => push_span(&mut rem, SpanKind::Changed, &text),
            ChangeTag::Insert => push_span(&mut add, SpanKind::Changed, &text),
        }
    }
    (
        DiffLine { kind: LineKind::Removed, spans: absorb_inner_gaps(rem) },
        DiffLine { kind: LineKind::Added, spans: absorb_inner_gaps(add) },
    )
}

/// `from_words` emits the separating whitespace as its own unchanged
/// token, so replacing two adjacent words yields Changed/Same/Changed —
/// one highlight per word with a visible seam between them. Absorb a
/// whitespace-only gap that sits between two changed runs, then merge.
/// Leading and trailing whitespace stays unchanged: it is not part of
/// what moved.
fn absorb_inner_gaps(spans: Vec<DiffSpan>) -> Vec<DiffSpan> {
    let mut out: Vec<DiffSpan> = Vec::with_capacity(spans.len());
    for (i, span) in spans.iter().enumerate() {
        let bridges_two_changes = span.kind == SpanKind::Same
            && !span.text.is_empty()
            && span.text.chars().all(char::is_whitespace)
            && out.last().is_some_and(|p| p.kind == SpanKind::Changed)
            && spans.get(i + 1).is_some_and(|n| n.kind == SpanKind::Changed);
        let kind = if bridges_two_changes { SpanKind::Changed } else { span.kind };
        push_span(&mut out, kind, &span.text);
    }
    out
}

/// Append text to the last span when it carries the same emphasis, so a
/// three-word replacement is one highlighted run instead of three
/// adjacent ones that render with visible seams.
fn push_span(spans: &mut Vec<DiffSpan>, kind: SpanKind, text: &str) {
    if let Some(last) = spans.last_mut() {
        if last.kind == kind {
            last.text.push_str(text);
            return;
        }
    }
    spans.push(DiffSpan { kind, text: text.to_string() });
}

/// Diff two texts into renderable lines. Replaced line pairs get word
/// granularity below `CHANGE_THRESHOLD` and plain full-line coloring at
/// or above it.
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    // Ops, not individual changes: an op keeps a replacement together as
    // a pair, and walking changes one at a time loses which removal the
    // following insertion answers. (`ops()` and not `grouped_ops()` —
    // nothing here elides context, and grouping with an unbounded window
    // overflows inside `similar`.)
    let mut out = Vec::new();
    for op in diff.ops() {
        let (removed, added) = op_sides(&diff, op);
        emit_op(&mut out, &removed, &added);
    }
    out
}

/// The old-side and new-side lines an op covers, newline-trimmed.
fn op_sides(diff: &TextDiff<'_, '_, '_, str>, op: &similar::DiffOp) -> (Vec<String>, Vec<String>) {
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for change in diff.iter_changes(op) {
        let text = change.value().trim_end_matches('\n').to_string();
        match change.tag() {
            ChangeTag::Equal => {
                removed.push(text.clone());
                added.push(text);
            }
            ChangeTag::Delete => removed.push(text),
            ChangeTag::Insert => added.push(text),
        }
    }
    (removed, added)
}

fn emit_op(out: &mut Vec<DiffLine>, removed: &[String], added: &[String]) {
    // Equal op: both sides identical, emit once as context.
    if removed == added {
        out.extend(removed.iter().map(|l| DiffLine::plain(LineKind::Context, l)));
        return;
    }
    // Pair up as far as both sides go; the tail is a pure add or delete.
    let paired = removed.len().min(added.len());
    for i in 0..paired {
        let (old, new) = (&removed[i], &added[i]);
        if change_ratio(old, new) < CHANGE_THRESHOLD {
            let (r, a) = word_spans(old, new);
            out.push(r);
            out.push(a);
        } else {
            // Too much moved for word granularity to read as anything
            // but confetti: colour the whole line and let the eye rest.
            out.push(DiffLine::plain(LineKind::Removed, old));
            out.push(DiffLine::plain(LineKind::Added, new));
        }
    }
    for line in &removed[paired..] {
        out.push(DiffLine::plain(LineKind::Removed, line));
    }
    for line in &added[paired..] {
        out.push(DiffLine::plain(LineKind::Added, line));
    }
}

/// `+3 -1` — the one-line shape of a diff, for a collapsed tool result.
#[cfg(test)]
pub fn diff_stat(lines: &[DiffLine]) -> String {
    let added = lines.iter().filter(|l| l.kind == LineKind::Added).count();
    let removed = lines.iter().filter(|l| l.kind == LineKind::Removed).count();
    format!("+{added} -{removed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(lines: &[DiffLine]) -> Vec<String> {
        lines.iter().map(|l| format!("{}{}", l.marker(), l.text())).collect()
    }

    #[test]
    fn identical_text_is_all_context() {
        let lines = diff_lines("a\nb\n", "a\nb\n");
        assert!(lines.iter().all(|l| l.kind == LineKind::Context));
        assert_eq!(diff_stat(&lines), "+0 -0");
    }

    #[test]
    fn a_one_word_rename_highlights_only_that_word() {
        let lines = diff_lines("let total = sum(xs);\n", "let count = sum(xs);\n");
        let removed = lines.iter().find(|l| l.kind == LineKind::Removed).expect("a removal");
        let added = lines.iter().find(|l| l.kind == LineKind::Added).expect("an addition");

        assert!(removed.has_intraline_highlight(), "word granularity, not a flat line");
        let changed: Vec<&str> = added
            .spans
            .iter()
            .filter(|s| s.kind == SpanKind::Changed)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(changed, vec!["count"], "only the renamed word is hot");
        // The unchanged remainder survives intact on both sides.
        assert_eq!(removed.text(), "let total = sum(xs);");
        assert_eq!(added.text(), "let count = sum(xs);");
    }

    #[test]
    fn a_rewritten_line_falls_back_to_full_line_coloring() {
        // Nothing shared: a word diff here would be pure confetti.
        let lines = diff_lines("alpha beta gamma delta\n", "one two three four\n");
        assert!(
            lines.iter().all(|l| !l.has_intraline_highlight()),
            "past the threshold, no intraline spans"
        );
        assert_eq!(rendered(&lines), vec!["-alpha beta gamma delta", "+one two three four"]);
    }

    #[test]
    fn the_threshold_is_where_the_fallback_starts() {
        // Under: one of four words moves (0.25-ish by char share).
        assert!(change_ratio("alpha beta gamma delta", "alpha beta gamma DELTA") < CHANGE_THRESHOLD);
        // Over: everything moves.
        assert!(change_ratio("alpha beta", "one two") >= CHANGE_THRESHOLD);
        // Degenerate inputs are identical, not maximally changed.
        assert_eq!(change_ratio("", ""), 0.0);
        assert_eq!(change_ratio("same", "same"), 0.0);
    }

    #[test]
    fn pure_insertions_and_deletions_keep_their_markers() {
        let lines = diff_lines("keep\n", "keep\nadded\n");
        assert_eq!(rendered(&lines), vec![" keep", "+added"]);
        assert_eq!(diff_stat(&lines), "+1 -0");

        let lines = diff_lines("keep\ngone\n", "keep\n");
        assert_eq!(rendered(&lines), vec![" keep", "-gone"]);
        assert_eq!(diff_stat(&lines), "+0 -1");
    }

    #[test]
    fn adjacent_changed_words_merge_into_one_run() {
        let lines = diff_lines("a b c d e f\n", "a X Y d e f\n");
        let added = lines.iter().find(|l| l.kind == LineKind::Added).expect("an addition");
        let runs = added.spans.iter().filter(|s| s.kind == SpanKind::Changed).count();
        assert_eq!(runs, 1, "one highlighted run, not one per word: {:?}", added.spans);
    }

    #[test]
    fn empty_sides_do_not_panic() {
        assert!(diff_lines("", "").is_empty());
        assert_eq!(diff_stat(&diff_lines("", "new\n")), "+1 -0");
        assert_eq!(diff_stat(&diff_lines("old\n", "")), "+0 -1");
    }

    #[test]
    fn multibyte_text_survives_word_granularity() {
        // Slicing by byte offset here used to be the classic panic.
        let lines = diff_lines("привет мир\n", "привет друг\n");
        let added = lines.iter().find(|l| l.kind == LineKind::Added).expect("an addition");
        assert_eq!(added.text(), "привет друг");
    }
}
