//! transcript — per-ingot session transcripts as JSONL, and mid-ingot
//! resume after a crash.
//!
//! The agent appends every `ChatMessage` (tool results included) to
//! `logs/transcripts/<ingot>-h<heat>.jsonl` as it forges. A process death
//! leaves the file without an `end` entry; on restart the forge finds the
//! Molten ingot, reloads the recorded messages, and resumes the agentic
//! loop instead of resetting to ore and burning a heat. Compaction
//! rewrites history wholesale, so it appends a `compact_boundary` entry
//! followed by a full redump — the reader only loads what follows the
//! last boundary.
//!
//! Readers are crash-tolerant (item 80): each line parses independently,
//! malformed lines are skipped with one warning, and a last line without
//! a trailing newline is a partial write and is dropped — the same guard
//! `crucible.rs` applies to ingot lines.
//!
//! Note: `ChatMessage::images` is `#[serde(skip)]`, so screenshots do not
//! survive a resume; the model re-captures them if it needs them.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::ChatMessage;

/// Transcript directory under the slag heap.
pub const TRANSCRIPT_DIR: &str = "logs/transcripts";

/// One JSONL line in a transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum Entry {
    /// One conversation message, in wire order.
    Msg { msg: ChatMessage },
    /// History was compacted: everything before this line is stale; a
    /// full redump of the compacted conversation follows.
    CompactBoundary,
    /// Session ended (success or failure). A transcript with this entry
    /// is never resumed.
    End { ok: bool },
    /// One crash-resume attempt started. Counting these bounds the
    /// re-spend loop when the crash cause is deterministic: past the cap
    /// the forge closes the transcript instead of resuming forever.
    ResumeAttempt,
}

// ─── Ingot scope (task-local) ───────────────────────────────────────────
//
// The forge knows the ingot id and heat; the agent (built deep inside
// `NativeSmith`) does not. A task-local carries the pair across the
// `Smith` boundary without threading it through every constructor:
// `strike_ingot` wraps the smith invocation in `scope`, and the agent
// reads `current()` from the same task.

tokio::task_local! {
    static INGOT_SCOPE: (String, u8);
}

/// Run `f` with (ingot id, heat) visible to `current()` on this task.
pub async fn scope<F: std::future::Future>(id: String, heat: u8, f: F) -> F::Output {
    INGOT_SCOPE.scope((id, heat), f).await
}

/// The (ingot id, heat) of the enclosing `scope`, if any.
pub fn current() -> Option<(String, u8)> {
    INGOT_SCOPE.try_with(|v| v.clone()).ok()
}

/// Filename-safe form of an ingot id. Ids come straight from PLAN.md
/// (LLM-written, prompt-injectable): path separators, traversal dots, and
/// other filesystem-hostile characters must not let a `:id` escape the
/// logs/ tree when it is interpolated into transcript/checkpoint names.
pub fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Transcript path for one attempt.
pub fn path_for(root: &Path, id: &str, heat: u8) -> PathBuf {
    root.join(TRANSCRIPT_DIR)
        .join(format!("{}-h{heat}.jsonl", sanitize_id(id)))
}

// ─── Writer ─────────────────────────────────────────────────────────────

/// Append-only transcript writer. IO errors are warned once and then
/// swallowed — recording must never kill the forge.
pub struct TranscriptWriter {
    path: PathBuf,
    warned: Mutex<bool>,
}

impl TranscriptWriter {
    pub fn new(path: PathBuf) -> Self {
        Self { path, warned: Mutex::new(false) }
    }

    /// Writer for the enclosing ingot `scope`, rooted at the workspace.
    /// `None` outside a scope (plan passes, tests, duel casts).
    pub fn for_current(root: &Path) -> Option<Self> {
        let (id, heat) = current()?;
        Some(Self::new(path_for(root, &id, heat)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Start a fresh attempt: truncate any stale content and record the
    /// opening messages (system + task).
    pub fn begin(&self, messages: &[ChatMessage]) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.path, b"") {
            self.warn(&e.to_string());
            return;
        }
        for msg in messages {
            self.record(msg);
        }
    }

    /// Append one message.
    pub fn record(&self, msg: &ChatMessage) {
        self.append(&Entry::Msg { msg: msg.clone() });
    }

    /// History was rewritten (compaction, summarization, steer injection):
    /// mark the boundary and redump the live conversation after it.
    pub fn boundary_and_redump(&self, messages: &[ChatMessage]) {
        self.append(&Entry::CompactBoundary);
        for msg in messages {
            self.record(msg);
        }
    }

    /// Close the transcript; a closed transcript is never resumed.
    pub fn end(&self, ok: bool) {
        self.append(&Entry::End { ok });
    }

    /// Record one crash-resume attempt (see `resume_attempts`).
    pub fn mark_resume(&self) {
        self.append(&Entry::ResumeAttempt);
    }

    fn append(&self, entry: &Entry) {
        let line = match serde_json::to_string(entry) {
            Ok(line) => line,
            Err(e) => {
                self.warn(&e.to_string());
                return;
            }
        };
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let write = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(line.as_bytes())?;
                f.write_all(b"\n")
            });
        if let Err(e) = write {
            self.warn(&e.to_string());
        }
    }

    fn warn(&self, err: &str) {
        let mut warned = self.warned.lock().unwrap_or_else(|p| p.into_inner());
        if !*warned {
            eprintln!("slag: transcript {} failed: {err}", self.path.display());
            *warned = true;
        }
    }
}

// ─── Reader ─────────────────────────────────────────────────────────────

/// Crash-tolerant JSONL parse: one `T` per newline-terminated line. A
/// last line without a trailing `\n` is a partial write and is dropped;
/// malformed lines are skipped (one warning per file).
pub(crate) fn read_jsonl_tolerant<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    // Drop the non-newline-terminated tail: it was cut mid-write.
    let complete = match raw.rfind('\n') {
        Some(at) => &raw[..=at],
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut warned = false;
    for line in complete.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(line) {
            Ok(v) => out.push(v),
            Err(e) => {
                if !warned {
                    eprintln!(
                        "slag: {}: skipped malformed line: {e}",
                        path.display()
                    );
                    warned = true;
                }
            }
        }
    }
    out
}

/// A transcript is resumable when it recorded real progress (at least one
/// message beyond system + task) and never wrote an `End` entry.
pub fn is_resumable(root: &Path, id: &str, heat: u8) -> bool {
    resumable_messages(&path_for(root, id, heat)).is_some()
}

/// How many crash-resume attempts this transcript has already absorbed.
pub fn resume_attempts(path: &Path) -> usize {
    read_jsonl_tolerant::<Entry>(path)
        .iter()
        .filter(|e| matches!(e, Entry::ResumeAttempt))
        .count()
}

/// Messages to resume from, or `None` when the transcript is absent,
/// closed, or recorded nothing worth resuming. Only entries after the
/// last `compact_boundary` count (the redump that follows it is the live
/// conversation). Dangling tool calls from a crash mid-batch get
/// synthetic error results so no `tool_call_id` is left unanswered.
pub fn resumable_messages(path: &Path) -> Option<Vec<ChatMessage>> {
    let entries: Vec<Entry> = read_jsonl_tolerant(path);
    if entries.is_empty() || entries.iter().any(|e| matches!(e, Entry::End { .. })) {
        return None;
    }
    let after_boundary = entries
        .iter()
        .rposition(|e| matches!(e, Entry::CompactBoundary))
        .map(|at| at + 1)
        .unwrap_or(0);
    let mut messages: Vec<ChatMessage> = entries[after_boundary..]
        .iter()
        .filter_map(|e| match e {
            Entry::Msg { msg } => Some(msg.clone()),
            _ => None,
        })
        .collect();
    // system + task alone is a session that never got a model response:
    // nothing to resume, a fresh start loses nothing.
    if messages.len() < 3 {
        return None;
    }
    backfill_dangling_tool_calls(&mut messages);
    Some(messages)
}

/// Every `tool_call_id` in an assistant message must have a tool result
/// after it, or strict backends reject the resumed request. A crash
/// between the assistant message and its tool results leaves the tail
/// dangling; synthesize interrupted-error results for the missing ids.
fn backfill_dangling_tool_calls(messages: &mut Vec<ChatMessage>) {
    let mut missing: Vec<String> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let Some(calls) = &m.tool_calls else { continue };
        for call in calls {
            let answered = messages[i + 1..]
                .iter()
                .any(|r| r.tool_call_id.as_deref() == Some(call.id.as_str()));
            if !answered {
                missing.push(call.id.clone());
            }
        }
    }
    for id in missing {
        messages.push(ChatMessage::tool_result(
            id,
            "ERROR: interrupted — slag restarted before this tool completed; \
             re-run it if its effect is still needed",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ToolCall;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn roundtrip_records_and_reloads_messages() {
        let d = dir();
        let path = path_for(d.path(), "i1", 2);
        let w = TranscriptWriter::new(path.clone());
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("task"),
            ChatMessage::assistant("thinking", None),
        ];
        w.begin(&msgs[..2]);
        w.record(&msgs[2]);

        let loaded = resumable_messages(&path).expect("resumable");
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].role, "system");
        assert_eq!(loaded[2].content, "thinking");
        assert!(is_resumable(d.path(), "i1", 2));
        assert!(!is_resumable(d.path(), "i1", 3), "other heats untouched");
    }

    #[test]
    fn end_entry_closes_the_transcript() {
        let d = dir();
        let path = path_for(d.path(), "i1", 1);
        let w = TranscriptWriter::new(path.clone());
        w.begin(&[
            ChatMessage::system("s"),
            ChatMessage::user("t"),
        ]);
        w.record(&ChatMessage::assistant("a", None));
        assert!(resumable_messages(&path).is_some());
        w.end(true);
        assert!(resumable_messages(&path).is_none(), "closed transcripts never resume");
    }

    #[test]
    fn begin_truncates_a_stale_transcript() {
        let d = dir();
        let path = path_for(d.path(), "i1", 1);
        let w = TranscriptWriter::new(path.clone());
        w.begin(&[ChatMessage::system("old"), ChatMessage::user("old task")]);
        w.record(&ChatMessage::assistant("old reply", None));
        // Fresh attempt at the same heat (transient retry): stale content
        // must not leak into the new session's record.
        w.begin(&[ChatMessage::system("new"), ChatMessage::user("new task")]);
        let entries: Vec<Entry> = read_jsonl_tolerant(&path);
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], Entry::Msg { msg } if msg.content == "new"));
    }

    /// Item 80: malformed lines are skipped and a truncated tail (no
    /// trailing newline) is treated as a partial write and dropped.
    #[test]
    fn reader_skips_bad_lines_and_truncated_tail() {
        let d = dir();
        let path = d.path().join("t.jsonl");
        let good = serde_json::to_string(&Entry::Msg { msg: ChatMessage::user("ok") }).unwrap();
        std::fs::write(
            &path,
            format!("{good}\nnot json at all\n{good}\n{{\"entry\":\"msg\",\"msg\":{{\"role\":\"user\",\"co"),
        )
        .unwrap();
        let entries: Vec<Entry> = read_jsonl_tolerant(&path);
        assert_eq!(entries.len(), 2, "bad line skipped, partial tail dropped");

        // A file that is nothing but a partial line parses to nothing.
        std::fs::write(&path, "{\"entry\":\"msg\"").unwrap();
        assert!(read_jsonl_tolerant::<Entry>(&path).is_empty());
        // Missing file: empty, no panic.
        assert!(read_jsonl_tolerant::<Entry>(&d.path().join("missing.jsonl")).is_empty());
    }

    #[test]
    fn resume_loads_only_post_boundary_context() {
        let d = dir();
        let path = path_for(d.path(), "i2", 1);
        let w = TranscriptWriter::new(path.clone());
        w.begin(&[ChatMessage::system("s"), ChatMessage::user("t")]);
        w.record(&ChatMessage::assistant("pre-compaction noise", None));
        // Compaction rewrote history: boundary + redump of the live view.
        let compacted = vec![
            ChatMessage::system("s"),
            ChatMessage::user("t"),
            ChatMessage::assistant("summary of earlier work", None),
        ];
        w.boundary_and_redump(&compacted);
        w.record(&ChatMessage::user("continue"));

        let loaded = resumable_messages(&path).expect("resumable");
        assert_eq!(loaded.len(), 4);
        assert!(loaded.iter().all(|m| m.content != "pre-compaction noise"));
        assert_eq!(loaded[3].content, "continue");
    }

    #[test]
    fn dangling_tool_calls_get_synthetic_interrupted_results() {
        let d = dir();
        let path = path_for(d.path(), "i3", 1);
        let w = TranscriptWriter::new(path.clone());
        let calls = vec![
            ToolCall { id: "c1".into(), name: "bash".into(), arguments: "{}".into() },
            ToolCall { id: "c2".into(), name: "read_file".into(), arguments: "{}".into() },
        ];
        w.begin(&[ChatMessage::system("s"), ChatMessage::user("t")]);
        w.record(&ChatMessage::assistant("", Some(calls)));
        // Crash landed after one of the two results was recorded.
        w.record(&ChatMessage::tool_result("c1", "done"));

        let loaded = resumable_messages(&path).expect("resumable");
        let results: Vec<&ChatMessage> = loaded.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(results.len(), 2, "every tool_call_id answered");
        let c2 = results
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c2"))
            .expect("synthetic result for the missing id");
        assert!(c2.content.contains("interrupted"), "{}", c2.content);
    }

    #[test]
    fn system_and_task_alone_are_not_worth_resuming() {
        let d = dir();
        let path = path_for(d.path(), "i4", 1);
        let w = TranscriptWriter::new(path.clone());
        w.begin(&[ChatMessage::system("s"), ChatMessage::user("t")]);
        assert!(resumable_messages(&path).is_none());
    }

    /// A hostile or malformed :id must not steer the transcript file
    /// outside logs/transcripts (path_for feeds create_dir_all + write).
    #[test]
    fn hostile_ingot_ids_cannot_escape_the_transcript_dir() {
        let d = dir();
        for id in ["../../../../tmp/pwn", "/etc/passwd", "a/b", "..", ""] {
            let p = path_for(d.path(), id, 1);
            let dir_part = p.parent().unwrap();
            assert!(
                dir_part.ends_with("logs/transcripts"),
                "id {id:?} escaped: {}",
                p.display()
            );
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(!name.contains('/') && !name.contains('\\'), "{name}");
        }
        assert_eq!(sanitize_id("i1"), "i1", "clean ids pass through");
        assert_eq!(sanitize_id("../../x"), ".._.._x");
        assert_eq!(sanitize_id(""), "_");
    }

    /// Resume markers count without disturbing resumability, so the forge
    /// can cap deterministic crash-resume loops.
    #[test]
    fn resume_attempts_count_and_do_not_break_resume() {
        let d = dir();
        let path = path_for(d.path(), "i6", 1);
        let w = TranscriptWriter::new(path.clone());
        w.begin(&[ChatMessage::system("s"), ChatMessage::user("t")]);
        w.record(&ChatMessage::assistant("progress", None));
        assert_eq!(resume_attempts(&path), 0);

        w.mark_resume();
        w.mark_resume();
        assert_eq!(resume_attempts(&path), 2);
        let loaded = resumable_messages(&path).expect("still resumable");
        assert_eq!(loaded.len(), 3, "markers are not messages");
        assert_eq!(resume_attempts(&d.path().join("missing.jsonl")), 0);
    }

    #[tokio::test]
    async fn scope_carries_the_ingot_context_within_the_task() {
        assert_eq!(current(), None);
        let seen = scope("i9".into(), 3, async { current() }).await;
        assert_eq!(seen, Some(("i9".into(), 3)));
        assert_eq!(current(), None, "scope must not leak past its future");
    }

    #[tokio::test]
    async fn writer_for_current_resolves_the_scoped_path() {
        let d = dir();
        let root = d.path().to_path_buf();
        let w = scope("i5".into(), 2, async move { TranscriptWriter::for_current(&root) }).await;
        let w = w.expect("writer inside scope");
        assert!(w.path().ends_with("logs/transcripts/i5-h2.jsonl"));
        assert!(TranscriptWriter::for_current(d.path()).is_none(), "no scope, no writer");
    }
}
