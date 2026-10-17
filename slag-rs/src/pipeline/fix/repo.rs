//! What `slag fix` learns about the repository, and the commands it runs
//! there: test detection, timed shell runs, and the git plumbing that turns
//! a verified fix into a reviewable branch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The repository's test command and where slag found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCmd {
    pub cmd: String,
    pub source: String,
}

impl TestCmd {
    fn new(cmd: impl Into<String>, source: impl Into<String>) -> Self {
        Self { cmd: cmd.into(), source: source.into() }
    }
}

/// The placeholder `npm init` writes. It is a test script that only fails.
const NPM_PLACEHOLDER: &str = "no test specified";

/// Find the test command from the manifests in `root`. Manifests win over
/// a Makefile: they name the runner directly, and a `make test` target
/// sometimes wants services a quick fix never needs.
pub fn detect_test(root: &Path) -> Option<TestCmd> {
    if let Some(t) = node_test(root) {
        return Some(t);
    }
    if root.join("Cargo.toml").is_file() {
        return Some(TestCmd::new("cargo test", "Cargo.toml"));
    }
    if root.join("go.mod").is_file() {
        return Some(TestCmd::new("go test ./...", "go.mod"));
    }
    if let Some(t) = python_test(root) {
        return Some(t);
    }
    if has_make_target(root, "test") {
        return Some(TestCmd::new("make test", "Makefile"));
    }
    None
}

fn node_test(root: &Path) -> Option<TestCmd> {
    let raw = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let script = json.pointer("/scripts/test").and_then(|s| s.as_str()).unwrap_or_default();
    if !script.trim().is_empty() && !script.contains(NPM_PLACEHOLDER) {
        // The lockfile names the package manager the repo actually uses.
        let runner = if root.join("pnpm-lock.yaml").is_file() && on_path("pnpm") {
            "pnpm"
        } else if root.join("yarn.lock").is_file() && on_path("yarn") {
            "yarn"
        } else {
            "npm"
        };
        return Some(TestCmd::new(format!("{runner} test"), "package.json scripts.test"));
    }
    // No script, but node:test files: the built-in runner finds them.
    has_file(root, 3, &|name| name.contains(".test.") && name.ends_with("js"))
        .then(|| TestCmd::new("node --test", "*.test.js files"))
}

fn python_test(root: &Path) -> Option<TestCmd> {
    let has_tests = has_file(root, 3, &|name| {
        name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py"))
    });
    let pytest_config = root.join("pytest.ini").is_file()
        || root.join("conftest.py").is_file()
        || file_mentions(&root.join("pyproject.toml"), "[tool.pytest")
        || file_mentions(&root.join("setup.cfg"), "[tool:pytest]")
        || file_mentions(&root.join("tox.ini"), "[pytest]");
    if !has_tests && !pytest_config {
        return None;
    }
    // A project virtualenv is the interpreter the tests were written for.
    let python = [".venv/bin/python", "venv/bin/python"]
        .iter()
        .find(|p| root.join(p).is_file())
        .map(|p| p.to_string())
        .unwrap_or_else(|| "python3".to_string());
    if pytest_config || imports(&python, root, "pytest") {
        return Some(TestCmd::new(format!("{python} -m pytest -q"), "pytest"));
    }
    // unittest discovery cannot start in a directory that is not a
    // package, so a bare tests/ is the start dir; a package is found from
    // the root like any other.
    let start = ["tests", "test"]
        .iter()
        .find(|d| root.join(d).is_dir() && !root.join(d).join("__init__.py").is_file());
    let cmd = match start {
        Some(dir) => format!("{python} -m unittest discover -s {dir}"),
        None => format!("{python} -m unittest discover"),
    };
    Some(TestCmd::new(cmd, "unittest discovery"))
}

fn has_make_target(root: &Path, target: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(root.join("Makefile")) else { return false };
    raw.lines().any(|l| {
        l.split_once(':')
            .is_some_and(|(t, rest)| t.trim() == target && !rest.starts_with('='))
    })
}

fn file_mentions(path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(needle))
}

/// The workspace picks what this runs: `python` may be its own
/// `.venv/bin/python`, and `import` reads a `pytest.py` beside it first.
/// So the probe runs in the sandbox, like the tests it chooses.
fn imports(python: &str, root: &Path, module: &str) -> bool {
    let real = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let sandbox = crate::engine::sandbox::Sandbox::for_root(&real);
    sandbox
        .wrap_std(python)
        .args(["-c", &format!("import {module}")])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Is `bin` an executable on PATH?
pub fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
    })
}

/// Directories no test or source search should descend into.
const SKIP_DIRS: &[&str] =
    &["node_modules", "target", "dist", "build", "venv", "__pycache__", "logs", "vendor"];

/// Is there a file within `depth` levels of `root` whose name matches?
fn has_file(root: &Path, depth: usize, pred: &dyn Fn(&str) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else { return false };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_file() && pred(&name) {
            return true;
        }
        if kind.is_dir()
            && depth > 0
            && !name.starts_with('.')
            && !SKIP_DIRS.contains(&name.as_str())
            && has_file(&entry.path(), depth - 1, pred)
        {
            return true;
        }
    }
    false
}

/// One command slag ran, and how it ended.
#[derive(Debug, Clone)]
pub struct Run {
    pub cmd: String,
    /// Exit code; `None` when the command was killed or never started.
    pub code: Option<i32>,
    pub output: String,
    pub secs: f64,
    pub timed_out: bool,
}

impl Run {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// `exit 0`, `exit 1`, `timed out after 300s`, or `killed`. A write
    /// the sandbox refused rides along: `exit 1 (write blocked by slag's
    /// sandbox: /path)`.
    pub fn status(&self) -> String {
        match (self.code, self.timed_out) {
            (_, true) => format!("timed out after {:.0}s", self.secs),
            (Some(code), _) => match crate::engine::sandbox::blocked_path(&self.output) {
                Some(path) => format!("exit {code} (write blocked by slag's sandbox: {path})"),
                None => format!("exit {code}"),
            },
            (None, _) => "killed".to_string(),
        }
    }

    /// The command could not run at all here: the shell found no such
    /// program, or it never finished. Neither says anything about the bug.
    pub fn unusable(&self) -> bool {
        self.timed_out || self.code == Some(127)
    }
}

/// Run `cmd` under bash in `dir`, stdout and stderr interleaved as a
/// terminal would show them. `CI=true` keeps watch-mode runners from
/// waiting on a keypress that never comes. The session can rewrite the
/// test scripts this runs, so it runs in the smith's shell sandbox; a
/// refused write ends the output with the sandbox's note.
pub async fn run(cmd: &str, dir: &Path, timeout: Duration) -> Run {
    let start = Instant::now();
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let sandbox = crate::engine::sandbox::Sandbox::for_root(&root);
    let mut command = sandbox.wrap("bash");
    command
        .arg("-c")
        .arg(format!("exec 2>&1\n{cmd}"))
        .current_dir(dir)
        .env("CI", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Run {
                cmd: cmd.to_string(),
                code: None,
                output: format!("could not start bash: {e}"),
                secs: 0.0,
                timed_out: false,
            }
        }
    };
    let pid = child.id();
    if let Some(pid) = pid {
        running().lock().unwrap_or_else(|e| e.into_inner()).push(pid);
    }
    let waited = tokio::time::timeout(timeout, child.wait_with_output()).await;
    if let Some(pid) = pid {
        running().lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != pid);
    }
    let secs = start.elapsed().as_secs_f64();
    match waited {
        Ok(Ok(out)) => {
            let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
            output.push_str(&String::from_utf8_lossy(&out.stderr));
            if let Some(note) = sandbox.denial_note(&output, !out.status.success()) {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&note);
            }
            Run { cmd: cmd.to_string(), code: out.status.code(), output, secs, timed_out: false }
        }
        Ok(Err(e)) => Run {
            cmd: cmd.to_string(),
            code: None,
            output: format!("wait failed: {e}"),
            secs,
            timed_out: false,
        },
        Err(_) => {
            // kill_on_drop reaches bash only; the test runner it started
            // lives in the same process group.
            if let Some(pid) = pid {
                kill_group(pid);
            }
            Run { cmd: cmd.to_string(), code: None, output: String::new(), secs, timed_out: true }
        }
    }
}

/// Process groups of the commands running right now. A Ctrl-C exits the
/// process without dropping anything, so `kill_on_drop` never fires there;
/// this list is what the shutdown path kills instead. `slag fix` registers
/// `kill_running` with the shutdown hooks; nothing here touches them, so
/// a unit test's commands cannot be killed by another test's shutdown.
fn running() -> &'static std::sync::Mutex<Vec<u32>> {
    static RUNNING: std::sync::OnceLock<std::sync::Mutex<Vec<u32>>> = std::sync::OnceLock::new();
    RUNNING.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Kill every command slag started and has not seen finish. Called before
/// any file is restored on the way out, so a test run cannot write over
/// the restore.
pub fn kill_running() {
    let pids = std::mem::take(&mut *running().lock().unwrap_or_else(|e| e.into_inner()));
    for pid in pids {
        kill_group(pid);
    }
}

/// The shell's builtin `kill`, not the binary: procps `kill` reads
/// `-KILL -<pid>` as a kill of every process the user can signal, so a
/// timed-out test took the whole session down on Linux.
fn kill_group(pid: u32) {
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill -9 -{pid}"))
        .stderr(Stdio::null())
        .status();
}

/// The last `lines` lines of `text`, capped at `bytes`. Test runners put
/// the verdict and the failing assertion at the end.
pub fn tail(text: &str, lines: usize, bytes: usize) -> String {
    let all: Vec<&str> = text.trim_end().lines().collect();
    let mut out = all[all.len().saturating_sub(lines)..].join("\n");
    if out.len() > bytes {
        let mut cut = out.len() - bytes;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        out = format!("...{}", &out[cut..]);
    }
    out
}

/// Build output and dependencies a test run writes. A repository without a
/// `.gitignore` for them still must not see them in the fix, and reading
/// a whole `target/` into a snapshot would cost more than the fix.
pub fn is_build_output(path: &str) -> bool {
    path.ends_with(".pyc")
        || path.split('/').any(|c| {
            matches!(c, "node_modules" | "target" | "__pycache__" | ".pytest_cache" | ".venv" | "venv")
        })
}

/// Does this path hold tests rather than the code under test?
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    parts.iter().any(|d| matches!(*d, "test" | "tests" | "__tests__" | "spec" | "specs"))
        || file.starts_with("test_")
        || file.contains("_test.")
        || file.contains(".test.")
        || file.contains(".spec.")
        || file.contains("_spec.")
        || file == "conftest.py"
}

/// Test definitions and skip markers in one test file, across the common
/// runners: pytest and unittest, node:test, jest and mocha, Rust, Go,
/// RSpec. A count, not a parse: enough to notice a test that vanished.
pub fn test_marks(text: &str) -> (usize, usize) {
    let def = regex::Regex::new(
        r#"(?m)^\s*(?:(?:async\s+)?def\s+test\w*\s*\(|(?:it|test)\s*\(\s*['"`]|#\[(?:tokio::)?test\b|func\s+Test\w*\s*\()"#,
    )
    .unwrap();
    let skip = regex::Regex::new(
        r#"(?m)@(?:unittest\.)?skip|@pytest\.mark\.skip|\b(?:it|test|describe)\.skip\s*\(|\bx(?:it|describe)\s*\(|#\[ignore\]|\bt\.Skip\("#,
    )
    .unwrap();
    (def.find_iter(text).count(), skip.find_iter(text).count())
}

/// A file's content before the session: `None` when it did not exist.
pub type Content = Option<Vec<u8>>;

/// Git state for one repository, addressed from its top level.
pub struct Git {
    pub top: PathBuf,
    /// The working directory, relative to `top` (`""` at the top, else
    /// `sub/dir/`). Scratch the engine writes lands here.
    pub prefix: String,
    /// For a directory outside git: a private repository in the temp dir
    /// whose one commit is the directory as it was before the session.
    /// It gives a plain directory the same change tracking and the same
    /// reproduction check, commits nothing for the user, and is deleted
    /// on drop.
    shadow: Option<PathBuf>,
}

/// What a shadow repository never tracks: build output, dependencies,
/// and the engine's own scratch.
const SHADOW_EXCLUDES: &str = "logs/\n.slag/\nnode_modules/\ntarget/\n__pycache__/\n*.pyc\n.venv/\nvenv/\n";
/// A directory with more files than this is not a project; shadowing it
/// would cost more than the fix.
const SHADOW_MAX_FILES: usize = 20_000;

impl Git {
    /// The repository containing `dir`, if any.
    pub fn open(dir: &Path) -> Option<Self> {
        let top = git_in(dir, &["rev-parse", "--show-toplevel"])?;
        let prefix = git_in(dir, &["rev-parse", "--show-prefix"]).unwrap_or_default();
        Some(Self { top: PathBuf::from(top.trim()), prefix: prefix.trim().to_string(), shadow: None })
    }

    /// Snapshot a directory that is not in git into a shadow repository.
    pub fn shadow(dir: &Path) -> Option<Self> {
        let top = dir.canonicalize().ok()?;
        let mut count = Vec::new();
        walk(&top, &top, 8, &mut count, SHADOW_MAX_FILES);
        if count.len() >= SHADOW_MAX_FILES {
            return None;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let home = std::env::temp_dir().join(format!("slag-fix-{}-{nanos}", std::process::id()));
        git_in(Path::new("."), &["init", "-q", &home.to_string_lossy()])?;
        let git_dir = home.join(".git");
        // The shadow sits in the temp dir, which model commands can write.
        crate::engine::sandbox::protect_git_dir(&git_dir);
        let _ = std::fs::write(git_dir.join("info").join("exclude"), SHADOW_EXCLUDES);
        let git = Self { top, prefix: String::new(), shadow: Some(git_dir) };
        git.git_checked(&["add", "-A"]).ok()?;
        git.git_checked(&[
            "-c",
            "user.name=slag",
            "-c",
            "user.email=slag@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--no-verify",
            "--allow-empty",
            "-m",
            "before slag fix",
        ])
        .ok()?;
        Some(git)
    }

    /// True for a shadow repository: there is no real repository to
    /// branch or commit in.
    pub fn is_shadow(&self) -> bool {
        self.shadow.is_some()
    }

    /// `git -C <dir>`, pointed at the shadow repository when there is one.
    fn command(&self, dir: &Path) -> std::process::Command {
        let mut cmd = crate::engine::sandbox::git();
        cmd.arg("-C").arg(dir);
        if let Some(git_dir) = &self.shadow {
            cmd.arg("--git-dir").arg(git_dir).arg("--work-tree").arg(&self.top);
        }
        cmd
    }

    pub fn git(&self, args: &[&str]) -> Option<String> {
        let out = self.command(&self.top).args(args).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Files git tracks under `dir`, relative to `dir`.
    pub fn ls_files(&self, dir: &Path) -> Option<String> {
        let out = self.command(dir).arg("ls-files").stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Run git and report its stderr on failure.
    pub fn git_checked(&self, args: &[&str]) -> Result<String, String> {
        let out = self
            .command(&self.top)
            .args(args)
            .env("GIT_LITERAL_PATHSPECS", "1")
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    /// Keep the engine's own scratch (`logs/`, `.slag/`) out of `git
    /// status` and out of the commit. `info/exclude` is local to this
    /// clone, so the repository's own `.gitignore` stays untouched.
    pub fn hide_scratch(&self) {
        let Some(path) = self.git(&["rev-parse", "--git-path", "info/exclude"]) else { return };
        let path = self.top.join(path.trim());
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let wanted: Vec<String> =
            ["logs/", ".slag/"].iter().map(|d| format!("/{}{d}", self.prefix)).collect();
        let missing: Vec<&String> =
            wanted.iter().filter(|w| !existing.lines().any(|l| l.trim() == w.as_str())).collect();
        if missing.is_empty() {
            return;
        }
        let mut add = String::new();
        if !existing.is_empty() && !existing.ends_with('\n') {
            add.push('\n');
        }
        add.push_str("# slag fix scratch\n");
        for w in missing {
            add.push_str(w);
            add.push('\n');
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, format!("{existing}{add}"));
    }

    /// Every path git reports as changed or untracked, relative to `top`.
    pub fn dirty_paths(&self) -> Vec<String> {
        let Some(raw) = self.git(&["status", "--porcelain", "-z", "--untracked-files=all"])
        else {
            return Vec::new();
        };
        parse_porcelain_z(&raw).into_iter().filter(|p| !is_build_output(p)).collect()
    }

    /// What the dirty files hold right now. Taken before the session so
    /// the user's own uncommitted work is never mistaken for the fix.
    pub fn snapshot(&self) -> BTreeMap<String, Content> {
        self.dirty_paths().into_iter().map(|p| (p.clone(), std::fs::read(self.top.join(&p)).ok())).collect()
    }

    /// Paths the session changed: dirty now, and not as they were in
    /// `before`.
    pub fn changed_since(&self, before: &BTreeMap<String, Content>) -> Vec<String> {
        self.dirty_paths()
            .into_iter()
            .filter(|p| match before.get(p) {
                Some(old) => *old != std::fs::read(self.top.join(p)).ok(),
                None => true,
            })
            .collect()
    }

    /// Paths that held uncommitted work before the session and are clean
    /// now: the session put them back to HEAD, and the work is gone from
    /// the working tree.
    pub fn reverted(&self, before: &BTreeMap<String, Content>) -> Vec<String> {
        let dirty = self.dirty_paths();
        before.keys().filter(|p| !dirty.contains(p)).cloned().collect()
    }

    /// A path's content before the session: the snapshot when it was
    /// already dirty, else HEAD, else absent.
    pub fn original(&self, before: &BTreeMap<String, Content>, path: &str) -> Content {
        if let Some(old) = before.get(path) {
            return old.clone();
        }
        let out = self
            .command(&self.top)
            .args(["show", &format!("HEAD:{path}")])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    }

    /// The session's changes as a diff, for a retry to start from.
    /// Untracked files are listed by name: `git diff` cannot show them
    /// without staging.
    pub fn diff_of(&self, paths: &[String]) -> String {
        let mut args = vec!["diff", "HEAD", "--"];
        args.extend(paths.iter().map(String::as_str));
        let mut diff = self.git(&args).unwrap_or_default();
        let tracked: Vec<String> = self
            .git(&["ls-files", "--", "."])
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let new: Vec<&String> = paths.iter().filter(|p| !tracked.contains(p)).collect();
        if !new.is_empty() {
            diff.push_str("\nnew files: ");
            diff.push_str(&new.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", "));
        }
        diff
    }

    /// `path +added -removed` per changed path, new files counted by line.
    pub fn stat(&self, paths: &[String]) -> Vec<String> {
        let mut args = vec!["diff", "HEAD", "--numstat", "--"];
        args.extend(paths.iter().map(String::as_str));
        let numstat = self.git(&args).unwrap_or_default();
        let mut counts: BTreeMap<String, (String, String)> = BTreeMap::new();
        for line in numstat.lines() {
            let mut cols = line.splitn(3, '\t');
            if let (Some(a), Some(d), Some(p)) = (cols.next(), cols.next(), cols.next()) {
                counts.insert(p.to_string(), (a.to_string(), d.to_string()));
            }
        }
        paths
            .iter()
            .map(|p| match counts.get(p) {
                Some((a, d)) => format!("{p} +{a} -{d}"),
                None => {
                    let lines = std::fs::read_to_string(self.top.join(p))
                        .map(|s| s.lines().count())
                        .unwrap_or(0);
                    format!("{p} +{lines} (new)")
                }
            })
            .collect()
    }

    /// `name`, or `name-2`, `name-3`... when a branch already has it.
    pub fn free_branch(&self, name: &str) -> String {
        let taken = |b: &str| self.git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_some();
        if !taken(name) {
            return name.to_string();
        }
        (2..100).map(|n| format!("{name}-{n}")).find(|b| !taken(b)).unwrap_or_else(|| {
            format!("{name}-{}", chrono::Local::now().format("%H%M%S"))
        })
    }

    /// Create `branch` at HEAD and commit exactly `paths` on it. Other
    /// staged or dirty work stays where it was, uncommitted.
    pub fn commit_on_branch(
        &self,
        branch: &str,
        paths: &[String],
        message: &str,
    ) -> Result<String, String> {
        // `git add` would overwrite a staged version the user has not
        // committed. Leave such a file alone rather than lose that version.
        let mut staged = vec!["diff", "--cached", "--name-only", "--"];
        staged.extend(paths.iter().map(String::as_str));
        let staged = self.git_checked(&staged)?;
        if !staged.trim().is_empty() {
            return Err(format!(
                "{} had staged changes before the fix; commit them or unstage them first",
                staged.split_whitespace().collect::<Vec<_>>().join(", ")
            ));
        }
        self.git_checked(&["switch", "-c", branch])
            .or_else(|_| self.git_checked(&["checkout", "-b", branch]))?;
        let mut add = vec!["add", "-A", "--"];
        add.extend(paths.iter().map(String::as_str));
        self.git_checked(&add)?;
        let mut commit = vec!["commit", "--quiet", "--only", "-m", message, "--"];
        commit.extend(paths.iter().map(String::as_str));
        self.git_checked(&commit)?;
        Ok(self.git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default().trim().to_string())
    }
}

impl Drop for Git {
    fn drop(&mut self) {
        if let Some(home) = self.shadow.as_ref().and_then(|d| d.parent()) {
            let _ = std::fs::remove_dir_all(home);
        }
    }
}

fn git_in(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::engine::sandbox::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Paths from `git status --porcelain -z`. A rename carries its source
/// path in the next field, and both paths changed; a copy's source did not.
fn parse_porcelain_z(raw: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = raw.split('\0');
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, path) = entry.split_at(3);
        paths.push(path.to_string());
        // A staged rename's source is part of the change too: committing
        // only the new path would leave the old one in HEAD.
        if xy.starts_with(['R', 'C']) {
            if let Some(from) = fields.next().filter(|_| xy.starts_with('R')) {
                paths.push(from.to_string());
            }
        }
    }
    paths
}

/// Code names the issue mentions: anything in backticks, plus words that
/// only code spells (`snake_case`, `camelCase`, `file.ext`, `call(`).
/// Plain English words carry no signal for finding the file.
pub fn issue_terms(text: &str) -> Vec<String> {
    let ident = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)*").unwrap();
    let ticks = regex::Regex::new(r"`([^`\n]+)`").unwrap();
    let mut terms = std::collections::BTreeSet::new();
    for span in ticks.captures_iter(text) {
        for m in ident.find_iter(&span[1]) {
            if m.as_str().len() >= 3 {
                terms.insert(m.as_str().to_string());
            }
        }
    }
    for m in ident.find_iter(text) {
        let t = m.as_str();
        let called = text[m.end()..].starts_with('(');
        let camel = t.chars().skip(1).any(|c| c.is_ascii_uppercase())
            && t.chars().any(|c| c.is_ascii_lowercase());
        if t.len() >= 3 && (t.contains('_') || t.contains('.') || camel || called) {
            terms.insert(t.to_string());
        }
    }
    terms.into_iter().collect()
}

/// The files that mention the most issue terms, best first. A file named
/// by the issue counts as a mention. Cheap enough to run on every fix: it
/// reads each listed file once and skips anything large.
pub fn rank_files(dir: &Path, files: &[String], terms: &[String], top: usize) -> Vec<String> {
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, &String)> = files
        .iter()
        .take(5_000)
        .filter_map(|f| {
            let path = dir.join(f);
            let small = std::fs::metadata(&path).is_ok_and(|m| m.len() <= 256 * 1024);
            let body = if small { std::fs::read_to_string(&path).unwrap_or_default() } else { String::new() };
            let hits = terms.iter().filter(|t| body.contains(t.as_str()) || f.contains(t.as_str())).count();
            (hits > 0).then_some((hits, f))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    scored.into_iter().take(top).map(|(_, f)| f.clone()).collect()
}

/// Tracked files under the working directory, as the smith's map of the
/// repository. Outside git, a shallow walk that skips build output.
pub fn file_list(dir: &Path, git: Option<&Git>) -> Vec<String> {
    if let Some(out) = git.and_then(|g| g.ls_files(dir)) {
        return out.lines().map(str::to_string).collect();
    }
    let mut files = Vec::new();
    walk(dir, dir, 4, &mut files, 2_000);
    files.sort();
    files
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>, cap: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if out.len() >= cap {
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() && depth > 0 && !SKIP_DIRS.contains(&name.as_str()) => {
                walk(root, &path, depth - 1, out, cap)
            }
            Ok(t) if t.is_file() => {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, body: &str) {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn detects_the_runner_each_ecosystem_names() {
        let node = tempfile::tempdir().unwrap();
        write(node.path(), "package.json", r#"{"scripts": {"test": "node --test"}}"#);
        assert_eq!(detect_test(node.path()).unwrap().cmd, "npm test");

        // The npm init placeholder is not a test suite.
        let placeholder = tempfile::tempdir().unwrap();
        write(
            placeholder.path(),
            "package.json",
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        );
        write(placeholder.path(), "test/a.test.js", "");
        assert_eq!(detect_test(placeholder.path()).unwrap().cmd, "node --test");

        let rust = tempfile::tempdir().unwrap();
        write(rust.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
        assert_eq!(detect_test(rust.path()).unwrap().cmd, "cargo test");

        let make = tempfile::tempdir().unwrap();
        write(make.path(), "Makefile", "CC := cc\ntest: build\n\t./run\n");
        assert_eq!(detect_test(make.path()).unwrap().cmd, "make test");

        assert!(detect_test(tempfile::tempdir().unwrap().path()).is_none());
    }

    #[test]
    fn python_unittest_starts_in_a_bare_tests_dir() {
        let py = tempfile::tempdir().unwrap();
        write(py.path(), "tests/test_x.py", "");
        let cmd = detect_test(py.path()).unwrap().cmd;
        // pytest when the machine has it, else stdlib discovery from tests/.
        assert!(
            cmd.ends_with("-m pytest -q") || cmd.ends_with("-m unittest discover -s tests"),
            "{cmd}"
        );

        let configured = tempfile::tempdir().unwrap();
        write(configured.path(), "pytest.ini", "[pytest]\n");
        write(configured.path(), "tests/test_x.py", "");
        assert_eq!(detect_test(configured.path()).unwrap().cmd, "python3 -m pytest -q");
    }

    #[test]
    fn test_paths_are_told_apart_from_source() {
        for p in [
            "tests/test_ledger.py",
            "test/duration.test.js",
            "src/__tests__/x.ts",
            "pkg/thing_test.go",
            "spec/model_spec.rb",
            "conftest.py",
        ] {
            assert!(is_test_path(p), "{p}");
        }
        for p in ["src/ledger.py", "lib/duration.js", "src/lib.rs", "contest.py", "latest.txt"] {
            assert!(!is_test_path(p), "{p}");
        }
    }

    #[test]
    fn issue_terms_are_the_code_names_not_the_prose() {
        let terms = issue_terms(
            "parseDuration ignores units. `export_csv(entries)` drops a day in ledger.py; \
             call median( with NaN.",
        );
        for t in ["parseDuration", "export_csv", "entries", "ledger.py", "median"] {
            assert!(terms.contains(&t.to_string()), "{t} missing from {terms:?}");
        }
        for t in ["ignores", "units", "drops", "call"] {
            assert!(!terms.contains(&t.to_string()), "{t} is prose: {terms:?}");
        }
    }

    #[test]
    fn files_that_mention_the_issue_rank_first() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/ledger.py", "def export_csv(entries): pass\n");
        write(dir.path(), "tests/test_ledger.py", "from src.ledger import export_csv\n");
        write(dir.path(), "README.md", "nothing here\n");
        let files: Vec<String> =
            ["README.md", "src/ledger.py", "tests/test_ledger.py"].map(String::from).to_vec();
        let terms = vec!["export_csv".to_string(), "entries".to_string()];
        assert_eq!(rank_files(dir.path(), &files, &terms, 5), ["src/ledger.py", "tests/test_ledger.py"]);
        assert!(rank_files(dir.path(), &files, &[], 5).is_empty());
    }

    #[test]
    fn test_marks_count_definitions_and_skips_per_runner() {
        let py = "class T(unittest.TestCase):\n    def test_a(self): pass\n    @unittest.skip('x')\n    def test_b(self): pass\n";
        assert_eq!(test_marks(py), (2, 1));
        let js = "test('parses', () => {});\nit.skip(\"later\", () => {});\ndescribe('x', () => {});\n";
        assert_eq!(test_marks(js), (1, 1));
        let rs = "#[test]\nfn a() {}\n#[tokio::test]\nasync fn b() {}\n#[test]\n#[ignore]\nfn c() {}\n";
        assert_eq!(test_marks(rs), (3, 1));
        assert_eq!(test_marks("func TestX(t *testing.T) { t.Skip(\"no\") }"), (1, 1));
    }

    #[test]
    fn porcelain_paths_include_rename_sources() {
        let raw = " M src/a.rs\0?? new file.txt\0R  b.rs\0old b.rs\0 D gone.rs\0";
        assert_eq!(
            parse_porcelain_z(raw),
            ["src/a.rs", "new file.txt", "b.rs", "old b.rs", "gone.rs"]
        );
    }

    #[test]
    fn tails_keep_the_end_and_respect_char_boundaries() {
        let text = (1..=10).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        assert_eq!(tail(&text, 2, 1_000), "line 9\nline 10");
        let wide = "ж".repeat(50);
        let cut = tail(&wide, 5, 11);
        assert!(cut.starts_with("..."), "{cut}");
    }

    #[tokio::test]
    async fn runs_report_exit_codes_output_and_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let ok = run("echo out; echo err >&2", dir.path(), Duration::from_secs(10)).await;
        assert!(ok.ok());
        assert!(ok.output.contains("out") && ok.output.contains("err"), "{}", ok.output);

        let missing = run("definitely-not-a-command-xyz", dir.path(), Duration::from_secs(10)).await;
        assert_eq!(missing.code, Some(127));
        assert!(missing.unusable());

        let slow = run("sleep 5", dir.path(), Duration::from_millis(200)).await;
        assert!(slow.timed_out && slow.unusable() && !slow.ok());
        assert!(slow.status().starts_with("timed out"), "{}", slow.status());
    }

    #[tokio::test]
    async fn a_timeout_kills_only_its_own_process_group() {
        let mut bystander = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let slow = run("sleep 5", dir.path(), Duration::from_millis(200)).await;
        let alive = bystander.try_wait().unwrap().is_none();
        let _ = bystander.kill();
        let _ = bystander.wait();
        assert!(slow.timed_out);
        assert!(alive, "the timeout's kill reached a process outside its group");
    }

    /// Test detection probes the workspace's own Python before any model
    /// call. A session can leave a `pytest.py` or a `.venv/bin/python`
    /// behind; the next `slag fix` must not run either outside the sandbox.
    #[test]
    fn test_detection_runs_planted_python_only_in_the_sandbox() {
        use crate::engine::sandbox::{backend, Backend};
        let (_guard, _cfg) = crate::config::isolated_config_dir();
        std::env::remove_var("SLAG_SANDBOX");
        if backend() == Backend::None {
            eprintln!("skipped: no sandbox backend works here");
            return;
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        for (planted, path, body) in [
            ("pyshadow", "pytest.py", "open(__import__('os').path.expanduser('~/slag-escape-probe-pyshadow'), 'w')\n"),
            ("venv", ".venv/bin/python", "#!/bin/sh\ntouch \"$HOME/slag-escape-probe-venv\"\n"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "tests/test_a.py", "import unittest\n");
            write(dir.path(), path, body);
            if planted == "venv" {
                use std::os::unix::fs::PermissionsExt;
                let script = dir.path().join(path);
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let probe = home.join(format!("slag-escape-probe-{planted}"));
            let _ = std::fs::remove_file(&probe);
            let detected = detect_test(dir.path());
            let leaked = probe.exists();
            let _ = std::fs::remove_file(&probe);
            assert!(!leaked, "{planted}: test detection ran planted code outside the sandbox");
            assert!(detected.is_some(), "{planted}: the tests are still found");
        }
    }

    /// The session can rewrite a test script; slag still runs it in the
    /// sandbox, and the attempt's status names the refused write.
    #[tokio::test]
    async fn a_test_script_that_writes_home_is_blocked_and_named() {
        use crate::engine::sandbox::{backend, Backend};
        let (_guard, _cfg) = crate::config::isolated_config_dir();
        std::env::remove_var("SLAG_SANDBOX");
        if backend() == Backend::None {
            eprintln!("skipped: no sandbox backend works here");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "test.sh", "set -e\ntouch \"$HOME/slag-sandbox-probe\"\necho tests ok\n");
        let probe = PathBuf::from(std::env::var_os("HOME").unwrap()).join("slag-sandbox-probe");
        let _ = std::fs::remove_file(&probe);
        let blocked = run("bash test.sh", dir.path(), Duration::from_secs(30)).await;
        let leaked = probe.exists();
        let _ = std::fs::remove_file(&probe);
        assert!(!leaked, "the test script escaped: {}", blocked.output);
        assert!(!blocked.ok());
        assert!(
            blocked
                .status()
                .ends_with(&format!("(write blocked by slag's sandbox: {})", probe.display())),
            "{}",
            blocked.status()
        );

        let fine = run("echo hi > out.txt && test -f out.txt", dir.path(), Duration::from_secs(30)).await;
        assert!(fine.ok(), "{}", fine.output);
    }

    fn repo() -> (tempfile::TempDir, Git) {
        let dir = tempfile::tempdir().unwrap();
        let sh = |cmd: &str| {
            let ok = std::process::Command::new("bash")
                .args(["-c", cmd])
                .current_dir(dir.path())
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "{cmd}");
        };
        sh("git init -q -b main && git config user.email t@t && git config user.name t");
        write(dir.path(), "src/a.py", "x = 1\n");
        write(dir.path(), "notes.txt", "mine\n");
        sh("git add -A && git commit -qm init");
        let git = Git::open(dir.path()).unwrap();
        (dir, git)
    }

    #[test]
    fn a_plain_directory_gets_a_shadow_that_tracks_the_session() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.py", "x = 1\n");
        write(dir.path(), "logs/old.jsonl", "{}\n");
        let git = Git::shadow(dir.path()).expect("a small directory is shadowed");
        assert!(git.is_shadow());
        assert!(!dir.path().join(".git").exists(), "the user's directory gets no .git");
        assert_eq!(file_list(dir.path(), Some(&git)), ["src/a.py"], "scratch is not tracked");

        let before = git.snapshot();
        write(dir.path(), "src/a.py", "x = 2\n");
        write(dir.path(), "tests/test_a.py", "assert True\n");
        write(dir.path(), "logs/engine.jsonl", "{}\n");
        assert_eq!(git.changed_since(&before), ["src/a.py", "tests/test_a.py"]);
        assert_eq!(git.original(&before, "src/a.py").as_deref(), Some(&b"x = 1\n"[..]));

        let home = git.shadow.clone().unwrap();
        drop(git);
        assert!(!home.exists(), "the shadow is deleted with its handle");
    }

    #[test]
    fn only_the_sessions_changes_reach_the_commit() {
        let (dir, git) = repo();
        git.hide_scratch();
        // The user's own work in progress, before the session starts.
        write(dir.path(), "notes.txt", "mine, edited\n");
        let before = git.snapshot();

        // The session: a fix, a new test, and engine scratch.
        write(dir.path(), "src/a.py", "x = 2\n");
        write(dir.path(), "tests/test_a.py", "assert True\n");
        write(dir.path(), "logs/engine-1.jsonl", "{}\n");
        write(dir.path(), "target/debug/junk", "build output\n");

        let changed = git.changed_since(&before);
        assert_eq!(changed, ["src/a.py", "tests/test_a.py"], "scratch and WIP stay out");
        assert_eq!(git.original(&before, "src/a.py").as_deref(), Some(&b"x = 1\n"[..]));
        assert_eq!(git.original(&before, "tests/test_a.py"), None);
        assert!(git.stat(&changed).iter().any(|l| l == "src/a.py +1 -1"), "{:?}", git.stat(&changed));

        let branch = git.free_branch("slag/fix-a");
        let sha = git.commit_on_branch(&branch, &changed, "fix: a").unwrap();
        assert!(!sha.is_empty());
        let committed = git.git(&["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert!(committed.contains("src/a.py") && committed.contains("tests/test_a.py"));
        assert!(!committed.contains("notes.txt"), "the user's WIP is not swept in");
        assert_eq!(
            git.git(&["status", "--porcelain"]).unwrap().trim(),
            "M notes.txt\n?? target/",
            "the WIP stays dirty, build output stays out, the scratch stays hidden"
        );
        assert_eq!(git.free_branch("slag/fix-a"), "slag/fix-a-2");
    }

    #[test]
    fn a_staged_version_the_user_has_not_committed_is_never_overwritten() {
        let (dir, git) = repo();
        let before = git.snapshot();
        write(dir.path(), "src/a.py", "x = 'staged'\n");
        let sh = |cmd: &str| {
            std::process::Command::new("bash").args(["-c", cmd]).current_dir(dir.path()).status().unwrap()
        };
        sh("git add src/a.py");
        write(dir.path(), "src/a.py", "x = 'fixed'\n");
        let changed = git.changed_since(&before);
        let err = git.commit_on_branch("slag/fix-a", &changed, "fix: a").unwrap_err();
        assert!(err.contains("src/a.py had staged changes"), "{err}");
        assert_eq!(git.git(&["branch", "--show-current"]).unwrap().trim(), "main");
        assert_eq!(git.git(&["show", ":src/a.py"]).unwrap(), "x = 'staged'\n", "index untouched");
    }
}
