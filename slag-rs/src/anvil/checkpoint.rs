//! checkpoint — file backups per forge attempt, and workspace rewind.
//!
//! Before the toolbox modifies a file for the first time in an attempt
//! (one heat of one ingot), the pre-modification bytes are stored
//! content-addressed under `logs/checkpoints/objects/` and a manifest
//! line is appended to `logs/checkpoints/<ingot>-h<heat>.jsonl`. When a
//! heat fails its proof and another heat remains, the forge rewinds the
//! workspace to the attempt-start snapshot so the retry starts clean
//! instead of building on the wreck of the failed attempt.
//!
//! The manifest is the snapshot: recording is lazy (only touched files
//! are backed up), objects dedupe by content hash, and the FIRST entry
//! per path wins on rewind — later entries are re-records of already
//! modified content. `rewind_attempt` / `rewind_latest` are the public
//! API the `slag rewind` CLI wires up.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::transcript::read_jsonl_tolerant;

/// Checkpoint store under the slag heap.
pub const CHECKPOINT_DIR: &str = "logs/checkpoints";

/// One manifest line: the pre-modification state of a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    /// Workspace-relative path.
    pub path: String,
    /// Content-hash object name, or `None` when the file did not exist
    /// at record time (rewind deletes it).
    pub object: Option<String>,
    #[serde(default)]
    pub size: u64,
}

/// Manifest key for one attempt. The id is sanitized the same way as
/// transcript filenames so a hostile `:id` cannot steer the manifest
/// outside `logs/checkpoints/`.
pub fn attempt_key(id: &str, heat: u8) -> String {
    format!("{}-h{heat}", crate::engine::transcript::sanitize_id(id))
}

fn manifest_path(root: &Path, key: &str) -> PathBuf {
    root.join(CHECKPOINT_DIR).join(format!("{key}.jsonl"))
}

fn objects_dir(root: &Path) -> PathBuf {
    root.join(CHECKPOINT_DIR).join("objects")
}

/// Per-attempt checkpoint recorder. Cheap to build; safe to build many
/// times for the same attempt (the manifest itself is the dedupe).
pub struct Checkpoint {
    root: PathBuf,
    key: String,
}

impl Checkpoint {
    pub fn new(root: impl Into<PathBuf>, key: impl Into<String>) -> Self {
        Self { root: root.into(), key: key.into() }
    }

    pub fn for_attempt(root: impl Into<PathBuf>, id: &str, heat: u8) -> Self {
        Self::new(root, attempt_key(id, heat))
    }

    /// The ingot this attempt belongs to, recovered from the key by
    /// dropping the `-h<heat>` suffix. Item 88 credits a write's churn to
    /// this id, so parallel anvils never cross-attribute: each carries its
    /// own checkpoint. Falls back to the whole key for a key that did not
    /// come from `attempt_key`.
    pub fn ingot_id(&self) -> &str {
        self.key
            .rsplit_once("-h")
            .filter(|(_, heat)| !heat.is_empty() && heat.bytes().all(|b| b.is_ascii_digit()))
            .map(|(id, _)| id)
            .unwrap_or(&self.key)
    }

    /// Attempt start: drop any stale manifest from a previous run of this
    /// key. Objects stay — they are content-addressed and shared.
    pub fn begin(&self) {
        let path = manifest_path(&self.root, &self.key);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, b"");
    }

    /// Back up `abs` before its first modification this attempt. Best
    /// effort: a failed backup must not fail the write it precedes.
    /// Re-records of an already-backed-up path are skipped (first entry
    /// wins on rewind anyway, so a race here costs bytes, not truth).
    pub fn record(&self, abs: &Path) {
        let manifest = manifest_path(&self.root, &self.key);
        let rel = abs
            .strip_prefix(&self.root)
            .unwrap_or(abs)
            .to_string_lossy()
            .into_owned();
        // Never checkpoint the checkpoint store or the slag heap itself.
        if rel.starts_with("logs/") || rel.starts_with("logs\\") {
            return;
        }
        // Never checkpoint the orchestrator's own state files: a rewind
        // that restored PLAN.md/PROGRESS.md to attempt start would wipe
        // status/heat transitions parallel anvils committed in between
        // (a forged sibling would flip back to ore and re-forge).
        if rel == crate::config::CRUCIBLE || rel == crate::config::LEDGER {
            return;
        }
        let existing: Vec<BackupEntry> = read_jsonl_tolerant(&manifest);
        if existing.iter().any(|e| e.path == rel) {
            return;
        }
        let entry = match std::fs::read(abs) {
            Ok(bytes) => {
                let object = format!("{:016x}", fnv64(&bytes));
                let dir = objects_dir(&self.root);
                let _ = std::fs::create_dir_all(&dir);
                let obj_path = dir.join(&object);
                if !obj_path.exists() && std::fs::write(&obj_path, &bytes).is_err() {
                    return; // no object, no manifest line — never a lying entry
                }
                BackupEntry { path: rel, object: Some(object), size: bytes.len() as u64 }
            }
            // Absent at record time: the attempt is creating it; rewind
            // deletes it.
            Err(_) => BackupEntry { path: rel, object: None, size: 0 },
        };
        let Ok(line) = serde_json::to_string(&entry) else { return };
        if let Some(parent) = manifest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&manifest)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(line.as_bytes())?;
                f.write_all(b"\n")
            });
    }

    /// Rewind this attempt's recorded files to their attempt-start state.
    pub fn rewind(&self) -> usize {
        rewind_attempt_key(&self.root, &self.key)
    }
}

/// Rewind one attempt by (ingot, heat). Returns files restored/removed.
pub fn rewind_attempt(root: &Path, id: &str, heat: u8) -> usize {
    rewind_attempt_key(root, &attempt_key(id, heat))
}

/// Whether an attempt was ever checkpointed. A restore count cannot make
/// the distinction `slag rewind --ingot X --heat N` needs: zero files
/// back from a real attempt is a no-op the operator can ignore, zero from
/// a mistyped id is a command that did nothing they asked for.
pub fn attempt_exists(root: &Path, id: &str, heat: u8) -> bool {
    manifest_path(root, &attempt_key(id, heat)).exists()
}

/// Join a manifest-supplied path under root, refusing absolute paths and
/// any `..` traversal. The manifest lives in the model-writable logs/
/// tree, so its lines are untrusted input: without this check a planted
/// entry could direct rewind to overwrite or delete files outside the
/// workspace sandbox that `ToolBox::resolve` enforces. `record` only ever
/// writes plain root-relative paths, so nothing legitimate is refused.
fn contained_target(root: &Path, rel: &str) -> Option<PathBuf> {
    use std::path::Component;
    let p = Path::new(rel);
    let mut target = root.to_path_buf();
    for comp in p.components() {
        match comp {
            Component::Normal(c) => target.push(c),
            Component::CurDir => {}
            _ => return None, // absolute, prefix, or `..` — escapes root
        }
    }
    (target != root).then_some(target)
}

/// Object names are content hashes: 16 hex chars. Anything else in a
/// manifest is a planted line trying to read outside the object store.
fn valid_object_name(object: &str) -> bool {
    object.len() == 16 && object.bytes().all(|b| b.is_ascii_hexdigit())
}

fn rewind_attempt_key(root: &Path, key: &str) -> usize {
    let manifest = manifest_path(root, key);
    let entries: Vec<BackupEntry> = read_jsonl_tolerant(&manifest);
    let mut seen: HashSet<String> = HashSet::new();
    let mut restored = 0usize;
    for entry in entries {
        // First entry per path wins: it is the attempt-start state.
        if !seen.insert(entry.path.clone()) {
            continue;
        }
        let Some(target) = contained_target(root, &entry.path) else {
            continue; // untrusted manifest line pointing outside root
        };
        match &entry.object {
            Some(object) => {
                if !valid_object_name(object) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(objects_dir(root).join(object)) else {
                    continue;
                };
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&target, &bytes).is_ok() {
                    restored += 1;
                }
            }
            None => {
                // Created during the attempt: remove it. Already gone
                // counts too — the goal state is "absent".
                if std::fs::remove_file(&target).is_ok() || !target.exists() {
                    restored += 1;
                }
            }
        }
    }
    restored
}

/// Rewind the most recently written attempt manifest (the `slag rewind`
/// default). Returns (attempt key, files restored) when one exists.
pub fn rewind_latest(root: &Path) -> Option<(String, usize)> {
    let dir = root.join(CHECKPOINT_DIR);
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(&dir).ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            newest = Some((mtime, stem.to_string()));
        }
    }
    let (_, key) = newest?;
    let restored = rewind_attempt_key(root, &key);
    Some((key, restored))
}

/// FNV-1a, matching `engine::tools::fnv64`'s object naming needs without
/// a cross-module dependency on a private helper.
fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn rewind_restores_modified_files_and_deletes_created_ones() {
        let d = dir();
        std::fs::write(d.path().join("keep.rs"), "original\n").unwrap();

        let c = Checkpoint::for_attempt(d.path(), "i1", 1);
        c.begin();
        // The attempt modifies keep.rs and creates fresh.rs.
        c.record(&d.path().join("keep.rs"));
        std::fs::write(d.path().join("keep.rs"), "mangled by a failed heat\n").unwrap();
        c.record(&d.path().join("fresh.rs"));
        std::fs::write(d.path().join("fresh.rs"), "half-written\n").unwrap();

        let restored = rewind_attempt(d.path(), "i1", 1);
        assert_eq!(restored, 2);
        assert_eq!(
            std::fs::read_to_string(d.path().join("keep.rs")).unwrap(),
            "original\n"
        );
        assert!(!d.path().join("fresh.rs").exists(), "created file removed");
    }

    #[test]
    fn first_entry_wins_across_re_records() {
        let d = dir();
        std::fs::write(d.path().join("a.txt"), "v0").unwrap();
        let c = Checkpoint::for_attempt(d.path(), "i1", 2);
        c.begin();
        c.record(&d.path().join("a.txt"));
        std::fs::write(d.path().join("a.txt"), "v1").unwrap();
        // A second recorder for the same attempt (transient re-invoke,
        // fresh ToolBox) sees the manifest and skips the re-record.
        let c2 = Checkpoint::for_attempt(d.path(), "i1", 2);
        c2.record(&d.path().join("a.txt"));
        std::fs::write(d.path().join("a.txt"), "v2").unwrap();

        assert_eq!(c.rewind(), 1);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "v0");
    }

    #[test]
    fn begin_truncates_a_stale_manifest_from_a_previous_run() {
        let d = dir();
        std::fs::write(d.path().join("a.txt"), "old-run").unwrap();
        let c = Checkpoint::for_attempt(d.path(), "i1", 1);
        c.begin();
        c.record(&d.path().join("a.txt"));

        // Same key, new run: begin wipes the old entries so the new
        // attempt's snapshot is not polluted by last run's state.
        std::fs::write(d.path().join("a.txt"), "new-run-start").unwrap();
        let c = Checkpoint::for_attempt(d.path(), "i1", 1);
        c.begin();
        c.record(&d.path().join("a.txt"));
        std::fs::write(d.path().join("a.txt"), "dirty").unwrap();

        rewind_attempt(d.path(), "i1", 1);
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "new-run-start"
        );
    }

    #[test]
    fn rewind_latest_picks_the_newest_manifest() {
        let d = dir();
        std::fs::write(d.path().join("a.txt"), "start").unwrap();
        let old = Checkpoint::for_attempt(d.path(), "i1", 1);
        old.begin();
        old.record(&d.path().join("a.txt"));

        std::fs::write(d.path().join("a.txt"), "mid").unwrap();
        let newer = Checkpoint::for_attempt(d.path(), "i2", 1);
        newer.begin();
        // Force a distinct mtime ordering.
        std::thread::sleep(std::time::Duration::from_millis(20));
        newer.record(&d.path().join("a.txt"));
        std::fs::write(d.path().join("a.txt"), "dirty").unwrap();

        let (key, restored) = rewind_latest(d.path()).expect("a manifest exists");
        assert_eq!(key, "i2-h1");
        assert_eq!(restored, 1);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "mid");
    }

    #[test]
    fn rewind_with_no_manifest_is_a_quiet_no_op() {
        let d = dir();
        assert_eq!(rewind_attempt(d.path(), "ghost", 1), 0);
        assert!(rewind_latest(d.path()).is_none());
    }

    /// A planted manifest line (the manifest lives under model-writable
    /// logs/) must not let rewind write or delete outside the workspace.
    #[test]
    fn rewind_refuses_manifest_paths_that_escape_the_root() {
        let outer = dir();
        let root = outer.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let victim = outer.path().join("victim.txt");
        std::fs::write(&victim, "outside the sandbox").unwrap();

        // Seed a real object so the traversal entries have bytes to plant.
        std::fs::write(root.join("inside.txt"), "v0").unwrap();
        let c = Checkpoint::for_attempt(&root, "i1", 1);
        c.begin();
        c.record(&root.join("inside.txt"));
        let object = {
            let entries: Vec<BackupEntry> =
                read_jsonl_tolerant(&manifest_path(&root, "i1-h1"));
            entries[0].object.clone().unwrap()
        };

        // Plant traversal + absolute + hostile-object lines by hand.
        let manifest = manifest_path(&root, "i1-h1");
        let mut planted = String::new();
        for entry in [
            BackupEntry { path: "../victim.txt".into(), object: Some(object.clone()), size: 2 },
            BackupEntry { path: "../victim.txt".into(), object: None, size: 0 },
            BackupEntry { path: victim.display().to_string(), object: None, size: 0 },
            BackupEntry {
                path: "planted.txt".into(),
                object: Some("../../victim.txt".into()),
                size: 2,
            },
        ] {
            planted.push_str(&serde_json::to_string(&entry).unwrap());
            planted.push('\n');
        }
        std::fs::write(&manifest, planted).unwrap();

        let restored = rewind_attempt(&root, "i1", 1);
        assert_eq!(restored, 0, "no planted entry may act");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "outside the sandbox",
            "file outside root untouched"
        );
        assert!(!root.join("planted.txt").exists(), "hostile object name refused");
    }

    /// The orchestrator's state files must survive a rewind: restoring
    /// PLAN.md to attempt start would wipe parallel anvils' transitions.
    #[test]
    fn crucible_and_ledger_are_never_checkpointed() {
        let d = dir();
        for name in [crate::config::CRUCIBLE, crate::config::LEDGER] {
            std::fs::write(d.path().join(name), "state").unwrap();
        }
        let c = Checkpoint::for_attempt(d.path(), "i1", 1);
        c.begin();
        c.record(&d.path().join(crate::config::CRUCIBLE));
        c.record(&d.path().join(crate::config::LEDGER));
        assert_eq!(c.rewind(), 0, "state files must not enter the manifest");
    }

    /// A hostile :id sanitizes into the manifest filename instead of
    /// creating directories outside logs/checkpoints.
    #[test]
    fn hostile_ids_cannot_escape_the_checkpoint_dir() {
        assert_eq!(attempt_key("../../tmp/pwn", 1), ".._.._tmp_pwn-h1");
        let d = dir();
        std::fs::write(d.path().join("a.txt"), "v0").unwrap();
        let c = Checkpoint::for_attempt(d.path(), "../../tmp/pwn", 1);
        c.begin();
        c.record(&d.path().join("a.txt"));
        assert!(d
            .path()
            .join(CHECKPOINT_DIR)
            .join(".._.._tmp_pwn-h1.jsonl")
            .exists());
    }

    #[test]
    fn slag_heap_files_are_never_checkpointed() {
        let d = dir();
        let log = d.path().join("logs").join("x.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "log line").unwrap();
        let c = Checkpoint::for_attempt(d.path(), "i1", 1);
        c.begin();
        c.record(&log);
        assert_eq!(c.rewind(), 0, "logs/ writes must not enter the manifest");
    }
}
