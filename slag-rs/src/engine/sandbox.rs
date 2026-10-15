//! sandbox: OS-level write confinement for the smith's shell commands.
//!
//! The file tools already refuse paths outside the anvil; this covers
//! `bash`, whose commands the model writes. Reads and network stay open.
//! Writes land only in the workspace, the temp dirs, the usual tool
//! caches under `$HOME`, and any `sandbox_writable` paths from config.
//!
//! Backends, picked once per process:
//! - macOS: `sandbox-exec` with a generated Seatbelt profile.
//! - Linux: `bwrap` when installed (whole fs read-only, writable paths
//!   bound read-write); else Landlock (kernel 5.13+, raw syscalls, no
//!   crate); else unconfined with one warning that names bubblewrap.
//! - anything else: unconfined, with the same one warning.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use tokio::process::Command;

use crate::config::SandboxMode;

/// Writable under `$HOME`: the caches build tools fill on a normal run.
const HOME_CACHES: &[&str] = &[
    ".cargo",
    ".rustup",
    ".npm",
    ".cache",
    "Library/Caches",
    ".gradle",
    ".m2",
    "go",
    ".bun",
    ".pnpm-store",
    ".local/share/pnpm",
    ".yarn",
];

/// Env vars that move those caches, or build output, somewhere else.
const CACHE_ENV: &[&str] = &[
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "RUSTUP_HOME",
    "GOPATH",
    "GOCACHE",
    "GOMODCACHE",
    "XDG_CACHE_HOME",
];

/// How a named denial opens and what follows its path. `blocked_path`
/// reads a note back with them, so the two stay in step.
const BLOCKED_NOTE: &str = "[sandbox] write outside the workspace was blocked by slag's sandbox: ";
const BLOCKED_TAIL: &str = ". Only the workspace, temp dirs and tool caches are writable; \
     write inside the workspace instead of retrying.";

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Allow everything, deny every write, then re-allow writes beneath the
/// `W<n>` params and to the devices a shell writes to. Seatbelt lets the
/// last matching rule win, so the order matters. Paths travel as `-D`
/// params, so none of them needs escaping inside the profile.
const SEATBELT_HEAD: &str =
    "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n";
const SEATBELT_DEVICES: &str = "    (literal \"/dev/null\")\n    (literal \"/dev/zero\")\n    \
     (literal \"/dev/stdout\")\n    (literal \"/dev/stderr\")\n    (literal \"/dev/ptmx\")\n    \
     (literal \"/dev/dtracehelper\")\n    (regex #\"^/dev/tty\")\n    (regex #\"^/dev/fd/\"))\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Seatbelt,
    Bwrap,
    Landlock,
    /// Unconfined: sandbox off, or no backend works here.
    None,
}

impl Backend {
    /// The errno text a denied write prints under this backend.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Backend::Seatbelt => &["operation not permitted"],
            Backend::Bwrap => &["read-only file system"],
            Backend::Landlock => &["permission denied"],
            Backend::None => &[],
        }
    }
}

/// One toolbox's confinement: the backend plus the paths it may write.
pub struct Sandbox {
    backend: Backend,
    writable: Vec<PathBuf>,
    /// Landlock ruleset, built once in the parent and applied in each
    /// child between fork and exec.
    #[cfg(target_os = "linux")]
    ruleset: Option<std::os::fd::OwnedFd>,
}

impl Sandbox {
    pub fn off() -> Self {
        Self::with(Backend::None, Vec::new())
    }

    /// The sandbox config asks for: `--no-sandbox`, `SLAG_SANDBOX`, then
    /// `sandbox` in config.toml, plus `sandbox_writable` extras.
    pub fn for_root(root: &Path) -> Self {
        let (mode, extras) = crate::config::sandbox_settings();
        match mode {
            SandboxMode::Off => Self::off(),
            SandboxMode::Auto => Self::new(root, &extras),
        }
    }

    /// Confine writes to `root`, temp, tool caches and `extras` with the
    /// backend this machine supports.
    pub fn new(root: &Path, extras: &[PathBuf]) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let tmpdir = std::env::var_os("TMPDIR").map(PathBuf::from);
        let env_caches: Vec<PathBuf> = CACHE_ENV
            .iter()
            .filter_map(|v| std::env::var_os(v))
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .collect();
        let writable = writable_paths(
            root,
            extras,
            home.as_deref(),
            tmpdir.as_deref(),
            &env_caches,
        );
        Self::with(backend(), writable)
    }

    fn with(backend: Backend, writable: Vec<PathBuf>) -> Self {
        #[cfg(target_os = "linux")]
        let (backend, ruleset) = match backend {
            // A ruleset the kernel refused leaves nothing to enforce.
            Backend::Landlock => match landlock::ruleset(&writable) {
                Some(fd) => (backend, Some(fd)),
                None => (Backend::None, None),
            },
            _ => (backend, None),
        };
        Self {
            backend,
            writable,
            #[cfg(target_os = "linux")]
            ruleset,
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// `sh -lc <script>`, wrapped in this sandbox.
    pub fn command(&self, script: &str) -> Command {
        let mut cmd = self.wrap("sh");
        cmd.arg("-lc").arg(script);
        cmd
    }

    /// `program` under this sandbox; the caller adds its arguments.
    pub fn wrap(&self, program: &str) -> Command {
        let mut cmd = match self.backend {
            Backend::Seatbelt => {
                let mut cmd = Command::new(SANDBOX_EXEC);
                cmd.args(self.seatbelt_args());
                cmd.arg(program);
                cmd
            }
            Backend::Bwrap => {
                let mut cmd = Command::new("bwrap");
                cmd.args(self.bwrap_args());
                cmd.arg("--").arg(program);
                cmd
            }
            Backend::Landlock | Backend::None => Command::new(program),
        };
        #[cfg(target_os = "linux")]
        if let Some(ruleset) = &self.ruleset {
            landlock::confine(&mut cmd, std::os::fd::AsRawFd::as_raw_fd(ruleset));
        }
        cmd
    }

    fn seatbelt_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> =
            vec!["-p".into(), seatbelt_profile(self.writable.len()).into()];
        for (i, path) in self.writable.iter().enumerate() {
            let mut define = OsString::from(format!("W{i}="));
            define.push(path.as_os_str());
            args.push("-D".into());
            args.push(define);
        }
        args
    }

    /// The whole filesystem read-only, the real `/dev` (null, ttys,
    /// `/dev/shm`), then each existing writable path bound read-write over
    /// it. bwrap refuses to bind a missing source, so absent caches drop.
    fn bwrap_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev"]
            .into_iter()
            .map(OsString::from)
            .collect();
        for path in self.writable.iter().filter(|p| p.exists()) {
            args.push("--bind".into());
            args.push(path.into());
            args.push(path.into());
        }
        args.push("--die-with-parent".into());
        args
    }

    /// When `output` shows the sandbox refused a write, the line the model
    /// needs so it changes approach instead of retrying blindly. A named
    /// path outside the writable set is reported plainly; a bare errno
    /// with no path gets a hedged note, and only when the command failed.
    pub fn denial_note(&self, output: &str, failed: bool) -> Option<String> {
        let markers = self.backend.markers();
        let mut unnamed = None;
        for line in output.lines() {
            let lower = line.to_ascii_lowercase();
            let Some((at, marker)) = markers
                .iter()
                .find_map(|m| lower.find(m).map(|at| (at, *m)))
            else {
                continue;
            };
            if reader_line(line) {
                continue;
            }
            match path_near(line, at) {
                Some(path) if !self.allows(&path) => {
                    return Some(format!("{BLOCKED_NOTE}{}{BLOCKED_TAIL}", path.display()));
                }
                // Inside the writable set: a real permission error, not ours.
                Some(_) => {}
                None => unnamed = unnamed.or(Some(marker)),
            }
        }
        let marker = unnamed.filter(|_| failed)?;
        Some(format!(
            "[sandbox] the command hit \"{marker}\". slag's sandbox blocks writes outside \
             the workspace, temp dirs and tool caches; if this was such a write, write inside \
             the workspace instead."
        ))
    }

    /// Whether the sandbox lets `path` be written. Missing tails resolve
    /// through their nearest existing ancestor, so `/tmp/new` matches the
    /// canonical `/private/tmp` entry. Relative paths start at the root,
    /// which is always the first writable entry.
    fn allows(&self, path: &Path) -> bool {
        let path = match self.writable.first() {
            Some(root) if path.is_relative() => resolve(&root.join(path)),
            _ => resolve(path),
        };
        path.starts_with("/dev") || self.writable.iter().any(|w| path.starts_with(w))
    }
}

/// The path a denial note in `output` names, for one-line summaries such
/// as a test run's status.
pub fn blocked_path(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let rest = line.strip_prefix(BLOCKED_NOTE)?;
        Some(rest.split_once(BLOCKED_TAIL).map_or(rest, |(path, _)| path))
    })
}

/// Probe once per process; the answer cannot change mid-run.
pub fn backend() -> Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    *BACKEND.get_or_init(probe)
}

/// Resolve the backend before any dashboard owns the terminal and print
/// the one warning a run gets when shell writes cannot be confined.
pub fn preflight() {
    if crate::config::sandbox_settings().0 == SandboxMode::Off {
        return;
    }
    if backend() == Backend::None {
        crate::tui::status_line("⚠", crate::tui::WARM, unconfined_warning());
    }
}

fn unconfined_warning() -> &'static str {
    if cfg!(target_os = "macos") {
        "sandbox: sandbox-exec failed here (already inside another sandbox?); \
         shell commands run unconfined. --no-sandbox silences this."
    } else if cfg!(target_os = "linux") {
        "sandbox: neither bubblewrap nor Landlock works here; shell commands can \
         write anywhere you can. Install bubblewrap (apt install bubblewrap, \
         dnf install bubblewrap) to confine them. --no-sandbox silences this."
    } else {
        "sandbox: no shell sandbox on this platform; shell commands run unconfined. \
         --no-sandbox silences this."
    }
}

/// Whether a probe command exits 0. A missing binary counts as failure.
fn runs(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn probe() -> Backend {
    if cfg!(target_os = "macos") {
        // Fails inside another Seatbelt sandbox (a nested agent harness).
        if runs(
            SANDBOX_EXEC,
            &["-p", "(version 1)(allow default)", "/usr/bin/true"],
        ) {
            return Backend::Seatbelt;
        }
        return Backend::None;
    }
    #[cfg(target_os = "linux")]
    {
        // Fails without unprivileged user namespaces (some containers,
        // Ubuntu's AppArmor restriction): fall through to Landlock.
        if runs(
            "bwrap",
            &["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "true"],
        ) {
            return Backend::Bwrap;
        }
        if landlock::abi().is_some() {
            return Backend::Landlock;
        }
    }
    Backend::None
}

fn seatbelt_profile(params: usize) -> String {
    let mut profile = String::from(SEATBELT_HEAD);
    for i in 0..params {
        profile.push_str(&format!("    (subpath (param \"W{i}\"))\n"));
    }
    profile.push_str(SEATBELT_DEVICES);
    profile
}

/// Every path the sandbox lets the shell write, deduplicated. Each entry
/// also appears canonicalized when that differs: Seatbelt matches resolved
/// paths (`/tmp` is `/private/tmp`), and a symlinked cache resolves too.
fn writable_paths(
    root: &Path,
    extras: &[PathBuf],
    home: Option<&Path>,
    tmpdir: Option<&Path>,
    env_caches: &[PathBuf],
) -> Vec<PathBuf> {
    let mut paths = vec![root.to_path_buf()];
    paths.extend(git_dirs(root));
    let temps: &[&str] = if cfg!(target_os = "macos") {
        &["/private/tmp", "/private/var/folders"]
    } else {
        &["/tmp", "/var/tmp"]
    };
    paths.extend(temps.iter().map(PathBuf::from));
    paths.extend(tmpdir.map(Path::to_path_buf));
    if let Some(home) = home {
        paths.extend(HOME_CACHES.iter().map(|c| home.join(c)));
    }
    paths.extend(env_caches.iter().cloned());
    paths.extend(extras.iter().cloned());

    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths.into_iter().filter(|p| p.is_absolute()) {
        let canonical = path.canonicalize().ok();
        for p in std::iter::once(path).chain(canonical) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// The git dir a workspace commits into when it sits outside the root:
/// a parent repo's `.git`, or a linked worktree's private and common
/// dirs. Without them `git add` in a subdirectory or a worktree fails.
fn git_dirs(root: &Path) -> Vec<PathBuf> {
    let Some(dot_git) = root
        .ancestors()
        .map(|a| a.join(".git"))
        .find(|g| g.exists())
    else {
        return Vec::new();
    };
    if dot_git.is_dir() {
        return if dot_git.starts_with(root) {
            Vec::new()
        } else {
            vec![dot_git]
        };
    }
    let Some(gitdir) = std::fs::read_to_string(&dot_git).ok().and_then(|s| {
        s.trim()
            .strip_prefix("gitdir:")
            .map(|g| g.trim().to_string())
    }) else {
        return Vec::new();
    };
    let base = dot_git.parent().unwrap_or(root);
    let gitdir = base.join(gitdir);
    let mut dirs = vec![gitdir.clone()];
    if let Ok(common) = std::fs::read_to_string(gitdir.join("commondir")) {
        dirs.push(gitdir.join(common.trim()));
    }
    dirs.into_iter()
        .map(|d| d.canonicalize().unwrap_or(d))
        .collect()
}

/// Output lines from tools that only read: macOS privacy (TCC) refuses
/// some reads with the same "Operation not permitted" text a sandboxed
/// write gets, and `find ~` hits it without writing anything.
fn reader_line(line: &str) -> bool {
    const READERS: &[&str] = &[
        "find:", "ls:", "du:", "grep:", "rg:", "cat:", "head:", "tail:", "stat:", "tree:", "wc:",
        "fd:", "file:",
    ];
    let first = line.split_whitespace().next().unwrap_or("");
    READERS.contains(&first)
}

/// The path an error line names next to its errno text at byte `at`: the
/// last path before it (`touch: /x: Operation not permitted`), else the
/// first after it (`PermissionError: … not permitted: '/x'`).
fn path_near(line: &str, at: usize) -> Option<PathBuf> {
    let mut before = None;
    let mut after = None;
    let mut start = None;
    for (i, c) in line
        .char_indices()
        .chain(std::iter::once((line.len(), ' ')))
    {
        if !c.is_whitespace() {
            start = start.or(Some(i));
            continue;
        }
        let Some(s) = start.take() else { continue };
        let token = line[s..i].trim_matches(|c: char| {
            matches!(
                c,
                '\'' | '"' | ':' | ',' | ';' | '(' | ')' | '[' | ']' | '`' | '‘' | '’'
            )
        });
        if !(token.starts_with('/') || token.starts_with("~/") || token.starts_with("../")) {
            continue;
        }
        if s < at {
            before = Some(token);
        } else if after.is_none() {
            after = Some(token);
        }
    }
    let token = before.or(after)?;
    Some(match token.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from)?.join(rest),
        None => PathBuf::from(token),
    })
}

/// Lexically clean `path`, then canonicalize its nearest existing
/// ancestor and re-attach the missing tail.
fn resolve(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other),
        }
    }
    let mut tail = Vec::new();
    let mut base = clean.as_path();
    loop {
        if let Ok(real) = base.canonicalize() {
            let mut out = real;
            out.extend(tail.iter().rev());
            return out;
        }
        match (base.file_name(), base.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                base = parent;
            }
            _ => return clean,
        }
    }
}

/// Landlock through raw syscalls: std links libc on Linux already, so the
/// three syscalls and `prctl` need no crate. Only write-side rights are
/// handled, which leaves reads and execs open everywhere.
#[cfg(target_os = "linux")]
mod landlock {
    use std::ffi::{c_int, c_long};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};

    // Same numbers on x86_64 and aarch64 (asm-generic table).
    const SYS_CREATE_RULESET: c_long = 444;
    const SYS_ADD_RULE: c_long = 445;
    const SYS_RESTRICT_SELF: c_long = 446;
    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: c_int = 1;
    const PR_SET_NO_NEW_PRIVS: c_int = 38;
    const O_PATH: c_int = 0o10000000;

    const WRITE_FILE: u64 = 1 << 1;
    const REMOVE_DIR: u64 = 1 << 4;
    const REMOVE_FILE: u64 = 1 << 5;
    const MAKE_CHAR: u64 = 1 << 6;
    const MAKE_DIR: u64 = 1 << 7;
    const MAKE_REG: u64 = 1 << 8;
    const MAKE_SOCK: u64 = 1 << 9;
    const MAKE_FIFO: u64 = 1 << 10;
    const MAKE_BLOCK: u64 = 1 << 11;
    const MAKE_SYM: u64 = 1 << 12;
    /// ABI 2: rename and link across directories.
    const REFER: u64 = 1 << 13;
    /// ABI 3: truncate.
    const TRUNCATE: u64 = 1 << 14;

    extern "C" {
        fn syscall(num: c_long, ...) -> c_long;
        fn prctl(option: c_int, ...) -> c_int;
    }

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    /// The kernel's Landlock ABI version, or None when it has none.
    pub fn abi() -> Option<c_long> {
        // SAFETY: the version query reads no memory (null attr, size 0).
        let v = unsafe {
            syscall(
                SYS_CREATE_RULESET,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        (v > 0).then_some(v)
    }

    /// A ruleset that handles every write right this kernel knows and
    /// grants them beneath each existing writable path and `/dev`.
    pub fn ruleset(writable: &[PathBuf]) -> Option<OwnedFd> {
        let abi = abi()?;
        let mut handled = WRITE_FILE
            | REMOVE_DIR
            | REMOVE_FILE
            | MAKE_CHAR
            | MAKE_DIR
            | MAKE_REG
            | MAKE_SOCK
            | MAKE_FIFO
            | MAKE_BLOCK
            | MAKE_SYM;
        if abi >= 2 {
            handled |= REFER;
        }
        if abi >= 3 {
            handled |= TRUNCATE;
        }
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: attr outlives the call and the size matches it.
        let fd = unsafe {
            syscall(
                SYS_CREATE_RULESET,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            return None;
        }
        // SAFETY: the kernel just returned this fd and nothing else owns it.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        let dev = PathBuf::from("/dev");
        for path in writable.iter().chain(std::iter::once(&dev)) {
            let Ok(file) = open_path(path) else { continue };
            let is_dir = file.metadata().is_ok_and(|m| m.is_dir());
            // Directory rights on a file rule are EINVAL.
            let allowed = if is_dir {
                handled
            } else {
                handled & (WRITE_FILE | TRUNCATE)
            };
            let rule = PathBeneathAttr {
                allowed_access: allowed,
                parent_fd: file.as_raw_fd(),
            };
            // SAFETY: rule outlives the call; both fds are open.
            let rc = unsafe {
                syscall(
                    SYS_ADD_RULE,
                    ruleset.as_raw_fd(),
                    RULE_PATH_BENEATH,
                    &rule as *const PathBeneathAttr,
                    0u32,
                )
            };
            if rc != 0 {
                return None;
            }
        }
        Some(ruleset)
    }

    fn open_path(path: &Path) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_PATH)
            .open(path)
    }

    /// Enforce the ruleset in the child between fork and exec. The parent
    /// keeps the fd open for as long as the sandbox lives.
    pub fn confine(cmd: &mut tokio::process::Command, ruleset: RawFd) {
        // SAFETY: the closure makes two raw syscalls and allocates nothing,
        // which is all a post-fork child may do.
        unsafe {
            cmd.pre_exec(move || {
                if prctl(PR_SET_NO_NEW_PRIVS, 1u64, 0u64, 0u64, 0u64) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if syscall(SYS_RESTRICT_SELF, ruleset, 0u32) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(backend: Backend, writable: &[&str]) -> Sandbox {
        Sandbox::with(backend, writable.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn profile_denies_writes_then_allows_each_param() {
        let profile = seatbelt_profile(2);
        let deny = profile.find("(deny file-write*)").expect("deny rule");
        let allow = profile.find("(allow file-write*").expect("allow rule");
        assert!(
            profile.starts_with("(version 1)\n(allow default)"),
            "{profile}"
        );
        assert!(deny < allow, "the allow must come last to win: {profile}");
        assert!(profile.contains("(subpath (param \"W0\"))"));
        assert!(profile.contains("(subpath (param \"W1\"))"));
        assert!(!profile.contains("W2"));
        for dev in ["\"/dev/null\"", "\"/dev/stdout\"", "#\"^/dev/tty\""] {
            assert!(profile.contains(dev), "{dev} missing: {profile}");
        }
        // Balanced parens, or sandbox-exec rejects the whole profile.
        assert_eq!(profile.matches('(').count(), profile.matches(')').count());
    }

    #[test]
    fn seatbelt_args_pass_paths_as_params() {
        let sb = sandbox(Backend::Seatbelt, &["/ws", "/private/tmp"]);
        let args: Vec<String> = sb
            .seatbelt_args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("(param \"W1\")"));
        assert_eq!(&args[2..], ["-D", "W0=/ws", "-D", "W1=/private/tmp"]);
    }

    #[test]
    fn bwrap_binds_root_read_only_then_existing_paths_writable() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_string_lossy().into_owned();
        let sb = sandbox(Backend::Bwrap, &[&ws, "/no/such/cache"]);
        let args: Vec<String> = sb
            .bwrap_args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[..6],
            ["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev"]
        );
        assert_eq!(&args[6..9], ["--bind", ws.as_str(), ws.as_str()]);
        assert!(
            !args.iter().any(|a| a == "/no/such/cache"),
            "missing sources drop: {args:?}"
        );
    }

    #[test]
    fn writable_set_covers_workspace_temp_caches_and_extras() {
        let root = tempfile::tempdir().unwrap();
        let home = Path::new("/home/smith");
        let paths = writable_paths(
            root.path(),
            &[
                PathBuf::from("/srv/data"),
                PathBuf::from("relative/ignored"),
            ],
            Some(home),
            Some(Path::new("/var/tmpdir-x")),
            &[PathBuf::from("/opt/cargo")],
        );
        assert_eq!(paths[0], root.path());
        for want in [
            "/home/smith/.cargo",
            "/home/smith/.rustup",
            "/home/smith/Library/Caches",
            "/home/smith/.local/share/pnpm",
            "/var/tmpdir-x",
            "/opt/cargo",
            "/srv/data",
        ] {
            assert!(
                paths.contains(&PathBuf::from(want)),
                "{want} missing: {paths:?}"
            );
        }
        let temp = if cfg!(target_os = "macos") {
            "/private/var/folders"
        } else {
            "/tmp"
        };
        assert!(paths.contains(&PathBuf::from(temp)), "{paths:?}");
        assert!(
            !paths.contains(&PathBuf::from("/home/smith")),
            "home itself stays read-only"
        );
        assert!(!paths.iter().any(|p| p.ends_with("relative/ignored")));
    }

    #[test]
    fn worktree_git_dirs_are_writable() {
        let dir = tempfile::tempdir().unwrap();
        let main_git = dir.path().join("main/.git");
        let private = main_git.join("worktrees/wt");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("commondir"), "../..\n").unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", private.display())).unwrap();
        let dirs = git_dirs(&wt);
        assert!(dirs.contains(&private.canonicalize().unwrap()), "{dirs:?}");
        assert!(dirs.contains(&main_git.canonicalize().unwrap()), "{dirs:?}");
        // A repo whose .git sits inside the workspace adds nothing.
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(git_dirs(&repo).is_empty());
    }

    #[test]
    fn denial_names_the_blocked_path_in_each_message_shape() {
        let sb = sandbox(Backend::Seatbelt, &["/ws"]);
        for out in [
            "touch: /Users/x/probe: Operation not permitted",
            "sh: /Users/x/probe: Operation not permitted",
            "/bin/sh: /Users/x/probe: Operation not permitted",
            "PermissionError: [Errno 1] Operation not permitted: '/Users/x/probe'",
            "Error: EPERM: operation not permitted, open '/Users/x/probe'",
        ] {
            let note = sb
                .denial_note(out, true)
                .unwrap_or_else(|| panic!("no note for {out}"));
            assert!(
                note.contains(
                    "write outside the workspace was blocked by slag's sandbox: /Users/x/probe."
                ),
                "{out} -> {note}"
            );
        }
        let bw = sandbox(Backend::Bwrap, &["/ws"]);
        let out = "touch: cannot touch '/home/x/probe': Read-only file system";
        assert!(bw
            .denial_note(out, true)
            .unwrap()
            .contains(": /home/x/probe."));
    }

    #[test]
    fn denial_ignores_writable_paths_reads_and_clean_output() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().canonicalize().unwrap();
        let sb = sandbox(Backend::Seatbelt, &[ws.to_str().unwrap()]);
        let inside = format!("chmod: {}/f: Operation not permitted", ws.display());
        assert_eq!(
            sb.denial_note(&inside, true),
            None,
            "a real EPERM inside the workspace"
        );
        let tcc = "find: /Users/x/Library/Mail: Operation not permitted";
        assert_eq!(sb.denial_note(tcc, true), None, "a privacy-refused read");
        assert_eq!(sb.denial_note("all good\n", true), None);
        assert_eq!(
            sandbox(Backend::None, &[]).denial_note("touch: /x: Operation not permitted", true),
            None
        );
    }

    #[test]
    fn blocked_path_reads_the_note_back() {
        let sb = sandbox(Backend::Seatbelt, &["/ws"]);
        let note = sb.denial_note("touch: /Users/x/probe: Operation not permitted", true).unwrap();
        let output = format!("running\n{note}\n");
        assert_eq!(blocked_path(&output), Some("/Users/x/probe"));
        assert_eq!(blocked_path("touch: /Users/x/probe: Operation not permitted"), None);
    }

    #[test]
    fn a_bare_errno_gets_a_hedged_note_only_on_failure() {
        let sb = sandbox(Backend::Seatbelt, &["/ws"]);
        let out = "error: failed to write lockfile: Operation not permitted (os error 1)";
        let note = sb.denial_note(out, true).expect("hedged note");
        assert!(note.contains("if this was such a write"), "{note}");
        assert_eq!(sb.denial_note(out, false), None);
    }

    #[test]
    fn allows_resolves_missing_tails_through_canonical_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().canonicalize().unwrap();
        let sb = sandbox(Backend::Seatbelt, &[ws.to_str().unwrap()]);
        // The raw tempdir path may be a symlink (/var -> /private/var).
        assert!(sb.allows(&dir.path().join("new/deeper/file")));
        assert!(sb.allows(&ws.join("a/../b")));
        assert!(!sb.allows(&ws.join("../escape")));
        assert!(sb.allows(Path::new("sub/new-file")), "relative paths start at the root");
        assert!(!sb.allows(Path::new("../escape")));
        assert!(sb.allows(Path::new("/dev/null")));
    }

    #[test]
    fn config_picks_the_backend_or_turns_it_off() {
        let (_guard, _cfg) = crate::config::isolated_config_dir();
        std::env::remove_var("SLAG_SANDBOX");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Sandbox::for_root(dir.path()).backend(),
            backend(),
            "auto is the default"
        );
        std::env::set_var("SLAG_SANDBOX", "off");
        let off = Sandbox::for_root(dir.path());
        std::env::remove_var("SLAG_SANDBOX");
        assert_eq!(off.backend(), Backend::None);
    }

    /// End to end through the bash tool: a write to `$HOME` is refused and
    /// named, while workspace, temp and toolchain commands keep working.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn seatbelt_blocks_home_writes_and_keeps_builds_working() {
        use super::super::tools::ToolBox;
        use super::super::ToolCall;

        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(&dir.path().canonicalize().unwrap(), &[]);
        if sb.backend() != Backend::Seatbelt {
            eprintln!("skipped: sandbox-exec does not work here (nested sandbox?)");
            return;
        }
        let tb = ToolBox::new(dir.path()).with_sandbox(sb);
        let bash = |command: &str| {
            let call = ToolCall {
                id: "t1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": command }).to_string(),
            };
            let tb = tb.clone();
            async move { tb.dispatch(&call).await.output }
        };

        let probe = PathBuf::from(std::env::var_os("HOME").unwrap()).join("slag-sandbox-probe");
        let _ = std::fs::remove_file(&probe);
        let out = bash("touch \"$HOME/slag-sandbox-probe\"").await;
        let leaked = probe.exists();
        let _ = std::fs::remove_file(&probe);
        assert!(!leaked, "the probe escaped the sandbox: {out}");
        assert!(
            out.contains(&format!(
                "write outside the workspace was blocked by slag's sandbox: {}",
                probe.display()
            )),
            "{out}"
        );

        let out = bash("touch ./ok && echo wrote").await;
        assert!(
            out.contains("wrote") && dir.path().join("ok").exists(),
            "{out}"
        );
        let out = bash("echo hi > \"$TMPDIR/slag-sandbox-tmp\" && rm \"$TMPDIR/slag-sandbox-tmp\" && echo tmp-ok").await;
        assert!(out.contains("tmp-ok"), "{out}");
        let out = bash("cargo --version").await;
        assert!(out.starts_with("cargo "), "{out}");
        if runs("python3", &["--version"]) {
            let out =
                bash("python3 -c 'import tempfile; tempfile.mkstemp(); print(\"py-ok\")'").await;
            assert!(out.contains("py-ok"), "{out}");
        }
    }

    /// The same contract on Linux, under whichever backend this machine has.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_backend_blocks_home_writes_and_keeps_workspace_writes() {
        use super::super::tools::ToolBox;
        use super::super::ToolCall;

        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(&dir.path().canonicalize().unwrap(), &[]);
        if sb.backend() == Backend::None {
            eprintln!("skipped: neither bwrap nor Landlock works here");
            return;
        }
        eprintln!("sandbox backend: {:?}", sb.backend());
        let tb = ToolBox::new(dir.path()).with_sandbox(sb);
        let bash = |command: &str| {
            let call = ToolCall {
                id: "t1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": command }).to_string(),
            };
            let tb = tb.clone();
            async move { tb.dispatch(&call).await.output }
        };
        let probe = PathBuf::from(std::env::var_os("HOME").unwrap()).join("slag-sandbox-probe");
        let _ = std::fs::remove_file(&probe);
        let out = bash("touch \"$HOME/slag-sandbox-probe\"").await;
        let leaked = probe.exists();
        let _ = std::fs::remove_file(&probe);
        assert!(!leaked, "the probe escaped the sandbox: {out}");
        assert!(out.contains("blocked by slag's sandbox"), "{out}");
        let out = bash("touch ./ok && echo hi > /tmp/slag-sandbox-tmp && rm /tmp/slag-sandbox-tmp && echo ws-ok").await;
        assert!(out.contains("ws-ok"), "{out}");
    }
}
