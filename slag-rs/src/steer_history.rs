//! Steer history: what the operator typed, kept across runs.
//!
//! A steer is the one thing in a forge the operator authors by hand, and
//! it is almost never a one-off. "focus on the failing proof" gets typed
//! again on the next heat, and again on the next run. Losing it to the
//! end of the process makes the operator retype prose they already wrote.
//!
//! Three properties the callers depend on, each of them a lesson from a
//! failure mode this module exists to avoid:
//!
//! * **The keypress path never touches the disk.** `record` pushes onto a
//!   process-global buffer and returns. A forge holding the history lock
//!   must not stall the typist mid-sentence, so the write is deferred to
//!   `flush`.
//! * **The append is exclusive but never unbounded.** Two forges in two
//!   terminals share one `~/.slag/history.jsonl`; interleaved appends
//!   corrupt lines. A lockfile serializes them, bounded retries keep a
//!   busy peer from wedging shutdown, and a lock older than `STALE_LOCK`
//!   is broken rather than waited on — otherwise one crashed forge takes
//!   history down forever.
//! * **Giving up loses nothing.** A flush that cannot claim the lock puts
//!   its entries back in the buffer for the next attempt.
//!
//! Recall is newest-first, deduped, and scoped to the project directory:
//! the steers typed in *this* repo are the ones worth pressing Up for.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long to keep trying for the lock before giving up and re-buffering.
/// Twenty rounds of 25ms is half a second — long enough to outlast a peer
/// appending a handful of lines, short enough that Ctrl-C still feels
/// instant when the peer is wedged.
const LOCK_RETRIES: usize = 20;
const LOCK_WAIT: Duration = Duration::from_millis(25);

/// A lockfile this old belongs to a process that is not coming back. Well
/// past any honest append, well short of a session.
const STALE_LOCK: Duration = Duration::from_secs(30);

/// One recalled steer. The field names match the record shape the
/// `_evidence:` line names, so a history file stays readable by eye.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// What the operator typed.
    pub display: String,
    /// The directory it was typed in — recall is scoped to this.
    pub project: String,
    /// Unix seconds. Ordering within the file is append order; this is
    /// for a human reading the JSONL, not for sorting.
    pub timestamp: u64,
}

/// Entries recorded this run and not yet on disk.
fn buffer() -> &'static Mutex<Vec<Entry>> {
    static BUFFER: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(Vec::new()))
}

/// Where history lives. `SLAG_HISTORY_FILE` overrides for tests and for
/// an operator who keeps state somewhere other than `$HOME` — the same
/// env-first shape `cli::sessions_dir` uses.
pub fn history_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SLAG_HISTORY_FILE") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".slag").join("history.jsonl"))
}

fn lock_path(history: &std::path::Path) -> PathBuf {
    let mut p = history.as_os_str().to_os_string();
    p.push(".lock");
    PathBuf::from(p)
}

/// The directory steers typed right now belong to. An unreadable cwd is
/// not fatal: the entry still records, it just recalls under `""`.
fn project() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Buffer a submitted steer. Cheap by construction: a lock on a Vec and a
/// push, no filesystem call on the path the user is typing down.
pub fn record(text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let entry = Entry {
        display: text.to_string(),
        project: project(),
        timestamp: now_secs(),
    };
    if let Ok(mut buf) = buffer().lock() {
        buf.push(entry);
    }
}

/// Claim the lockfile, or report that a live peer holds it.
///
/// `create_new` is the whole exclusion mechanism: the OS decides the
/// winner, so two forges racing cannot both believe they won. A lock left
/// by a crashed process gets broken once it is `STALE_LOCK` old, because
/// the alternative is history that never appends again.
fn claim_lock(lock: &std::path::Path) -> bool {
    for attempt in 0..LOCK_RETRIES {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock)
        {
            Ok(_) => return true,
            Err(_) => {
                // Break a lock whose owner is plainly gone. Checked on
                // every round, not just the first: a lock can age past
                // the threshold while we wait on it.
                let stale = std::fs::metadata(lock)
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().map(|e| e > STALE_LOCK).unwrap_or(false))
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(lock);
                    continue;
                }
                if attempt + 1 < LOCK_RETRIES {
                    std::thread::sleep(LOCK_WAIT);
                }
            }
        }
    }
    false
}

/// Append everything buffered to the history file, one JSON object per
/// line. Returns how many entries landed.
///
/// Safe to call on any exit path and safe to call twice: the buffer is
/// taken, so a second call with nothing pending writes nothing. Anything
/// that did not reach the file goes back where it came from — a lock a
/// peer holds, a file that will not open, or a write that dies partway
/// through the batch. Dropping an operator's prose to save a shutdown a
/// few milliseconds is the one outcome worse than a slow shutdown.
pub fn flush() -> usize {
    let pending: Vec<Entry> = match buffer().lock() {
        Ok(mut buf) => std::mem::take(&mut *buf),
        Err(_) => return 0,
    };
    if pending.is_empty() {
        return 0;
    }
    let Some(path) = history_path() else {
        return 0;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let lock = lock_path(&path);

    let restore = |entries: Vec<Entry>| {
        if let Ok(mut buf) = buffer().lock() {
            // Front, not back: these were recorded before anything the
            // buffer picked up while the flush was in flight.
            let mut merged = entries;
            merged.append(&mut buf);
            *buf = merged;
        }
    };

    if !claim_lock(&lock) {
        restore(pending);
        return 0;
    }
    let (written, owed) = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            let mut n = 0;
            let mut owed: Vec<Entry> = Vec::new();
            for (i, entry) in pending.iter().enumerate() {
                let Ok(line) = serde_json::to_string(entry) else {
                    // Unserializable now is unserializable forever, so
                    // re-buffering this one would retry the same failure
                    // on every exit path. Skip it and keep the rest.
                    continue;
                };
                if writeln!(f, "{line}").is_err() {
                    // A full disk or a revoked mount, mid-append. The
                    // entries past the failure never reached the file and
                    // are still owed to the operator.
                    owed = pending[i..].to_vec();
                    break;
                }
                n += 1;
            }
            let _ = f.flush();
            (n, owed)
        }
        Err(_) => (0, pending),
    };
    let _ = std::fs::remove_file(&lock);
    if !owed.is_empty() {
        restore(owed);
    }
    written
}

/// Steers to walk with the Up arrow: newest first, deduped, scoped to the
/// current project.
///
/// Unflushed entries come first, so a steer typed a moment ago recalls
/// without waiting for shutdown. Dedupe keeps the first occurrence seen,
/// which after the newest-first reversal is the most recent — pressing Up
/// four times should reach four *different* steers, not the same one the
/// operator sent on every heat.
pub fn recall() -> Vec<String> {
    let here = project();
    let mut ordered: Vec<String> = Vec::new();

    if let Ok(buf) = buffer().lock() {
        for e in buf.iter().rev() {
            if e.project == here {
                ordered.push(e.display.clone());
            }
        }
    }
    if let Some(path) = history_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines().rev() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // A truncated or hand-edited line is skipped, never fatal:
                // history is a convenience, and refusing to recall
                // anything because one line is malformed helps nobody.
                if let Ok(e) = serde_json::from_str::<Entry>(line) {
                    if e.project == here {
                        ordered.push(e.display);
                    }
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    ordered.retain(|d| seen.insert(d.clone()));
    ordered
}

/// Register the flush with the shutdown registry, once per process, so
/// Ctrl-C and the panic hook both land the last steer. Idempotent: the
/// dashboard calls it on every attach.
pub fn install_flush() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::shutdown::register(|| {
            flush();
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // `HOME`, the cwd, and the buffer are all process-global, so these
    // tests serialize against one another.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Point history at a scratch file and start from an empty buffer.
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.jsonl");
        std::env::set_var("SLAG_HISTORY_FILE", &path);
        if let Ok(mut buf) = buffer().lock() {
            buf.clear();
        }
        (dir, path)
    }

    #[test]
    fn flush_appends_jsonl_and_releases_the_lock() {
        let _g = guard();
        let (_dir, path) = scratch();

        record("focus on the failing proof");
        record("stop rewriting the test");
        assert_eq!(flush(), 2);

        let text = std::fs::read_to_string(&path).expect("history written");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one JSON object per line");
        let first: Entry = serde_json::from_str(lines[0]).expect("parses");
        assert_eq!(first.display, "focus on the failing proof");
        assert_eq!(first.project, project());
        assert!(
            !lock_path(&path).exists(),
            "the lock is released, not leaked to the next run"
        );

        // A second flush with nothing pending is a no-op, so registering
        // it on several exit paths cannot duplicate lines.
        assert_eq!(flush(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
    }

    #[test]
    fn a_held_lock_gives_up_bounded_and_keeps_the_buffer() {
        let _g = guard();
        let (_dir, path) = scratch();
        // A peer mid-append: fresh lock, so it is honored rather than broken.
        let lock = lock_path(&path);
        std::fs::write(&lock, b"").expect("hold the lock");

        record("do not lose me");
        let started = std::time::Instant::now();
        assert_eq!(flush(), 0, "the peer owns the file");
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(5),
            "bounded retries, not an unbounded spin (waited {waited:?})"
        );
        assert!(!path.exists(), "nothing was written under someone else's lock");

        // The entry survived the failed attempt and lands once the peer leaves.
        std::fs::remove_file(&lock).unwrap();
        assert_eq!(flush(), 1);
        assert!(std::fs::read_to_string(&path).unwrap().contains("do not lose me"));
    }

    #[test]
    fn a_stale_lock_is_broken_not_waited_on() {
        let _g = guard();
        let (_dir, path) = scratch();
        let lock = lock_path(&path);
        // Backdate past the threshold: the owner crashed and is not coming back.
        let old = SystemTime::now() - (STALE_LOCK + Duration::from_secs(60));
        let f = std::fs::File::create(&lock).expect("stale lock");
        f.set_modified(old).expect("backdate the lock");
        drop(f);

        record("a crashed forge must not wedge history");
        assert_eq!(flush(), 1, "the dead owner's lock was broken");
        assert!(!lock.exists());
    }

    #[test]
    fn recall_is_newest_first_deduped_and_project_scoped() {
        let _g = guard();
        let (_dir, path) = scratch();
        let here = project();
        let lines = [
            Entry { display: "oldest".into(), project: here.clone(), timestamp: 1 },
            Entry { display: "repeat".into(), project: here.clone(), timestamp: 2 },
            Entry { display: "elsewhere".into(), project: "/some/other/repo".into(), timestamp: 3 },
            Entry { display: "repeat".into(), project: here.clone(), timestamp: 4 },
            Entry { display: "newest".into(), project: here.clone(), timestamp: 5 },
        ];
        let body: String = lines
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
            .collect();
        std::fs::write(&path, body).unwrap();

        assert_eq!(recall(), vec!["newest", "repeat", "oldest"]);

        // A steer typed this run recalls before shutdown flushes it.
        record("just typed");
        assert_eq!(recall()[0], "just typed");
    }

    #[test]
    fn a_malformed_line_does_not_sink_the_rest_of_the_history() {
        let _g = guard();
        let (_dir, path) = scratch();
        let good = serde_json::to_string(&Entry {
            display: "survivor".into(),
            project: project(),
            timestamp: 1,
        })
        .unwrap();
        std::fs::write(&path, format!("{{ truncated\n{good}\n")).unwrap();
        assert_eq!(recall(), vec!["survivor"]);
    }

    /// The lock is not the only way a flush fails. A history path that
    /// will not open owes the operator the same thing a busy peer does:
    /// every entry back in the buffer for the next attempt.
    #[test]
    fn an_unopenable_history_file_keeps_the_buffer() {
        let _g = guard();
        let (dir, path) = scratch();
        // A directory where the file should be: `open` fails, and it
        // fails the same way on every platform.
        std::fs::create_dir(&path).expect("occupy the history path");

        record("still owed");
        assert_eq!(flush(), 0, "nothing landed");
        assert!(!lock_path(&path).exists(), "the lock is released anyway");

        // Point at a writable path and the entry is still there to land.
        let good = dir.path().join("second-try.jsonl");
        std::env::set_var("SLAG_HISTORY_FILE", &good);
        assert_eq!(flush(), 1);
        assert!(std::fs::read_to_string(&good).unwrap().contains("still owed"));
    }

    #[test]
    fn record_ignores_blank_input() {
        let _g = guard();
        let (_dir, _path) = scratch();
        record("   ");
        record("");
        assert_eq!(flush(), 0, "whitespace is not a steer");
    }
}
