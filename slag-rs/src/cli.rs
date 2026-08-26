use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::error::SlagError;
use crate::pipeline::{fg, reset};
use crate::tui;

#[derive(Parser)]
#[command(
    name = "slag",
    about = "Smelt ideas, skim the bugs, forge the product.",
    version,
    author
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Commission (new project description)
    #[arg(trailing_var_arg = true)]
    pub commission: Vec<String>,

    /// Branch-per-ingot worktree isolation (not implemented yet; ignored
    /// with a warning — all ingots run in the shared checkout)
    #[arg(long)]
    pub worktree: bool,

    /// Full-screen dashboard (crucible, live feed, steering input)
    #[arg(long)]
    pub tui: bool,

    /// Judge the finished artifact against the goal, and forge again on a
    /// loss. A fresh critic that built nothing opens the real thing,
    /// compares it with the bar in BAR.md, and turns the biggest gap into
    /// the next ingots. 0 (default) leaves the goal unjudged.
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub temper: usize,

    /// Commission a project whose whole description is one word. Without
    /// it a lone bare word is refused as a mistyped subcommand, since that
    /// is what it nearly always is.
    #[arg(long)]
    pub force: bool,

    /// Max parallel anvil workers
    #[arg(long, default_value_t = crate::config::MAX_ANVILS)]
    pub anvils: usize,

    /// Route every role through openrouter/auto, ignoring models pinned in
    /// the environment or config file. This is already the default; the
    /// flag exists to override those pins. Explicit model flags still win.
    #[arg(long)]
    pub auto: bool,

    /// Worker model, any OpenRouter id (overrides SLAG_MODEL_BASE)
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Planner model for grade>=3 ingots (overrides SLAG_MODEL_PLAN)
    #[arg(long, value_name = "MODEL")]
    pub plan_model: Option<String>,

    /// Duel judge model (overrides SLAG_MODEL_JUDGE)
    #[arg(long, value_name = "MODEL")]
    pub judge_model: Option<String>,

    /// Force multi-cast forging: every solo ingot gets at least two casts
    /// (equivalent to SLAG_DUEL=on)
    #[arg(long)]
    pub duel: bool,

    /// Write a Chrome trace-event file beside the JSONL log: one lane per
    /// anvil, tool calls nested inside their ingot. Load it in
    /// chrome://tracing or Perfetto to see what ran in parallel.
    #[arg(long, value_name = "FILE")]
    pub trace: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show or set the OpenRouter key (the only setup slag needs)
    Key {
        /// Key to verify and save; omit to show the current setup
        #[arg(value_name = "KEY")]
        key: Option<String>,
    },

    /// Show crucible state
    Status {
        /// One JSON object for external consumers (tmux statuslines, CI,
        /// the website): run id, ingots by status, spend, tokens, last
        /// event — read from the live event log and persisted costs.
        #[arg(long)]
        json: bool,
    },

    /// List past runs from the event logs in logs/
    Runs,

    /// Offline analytics over the logs/ heap: ingots forged/cracked,
    /// heats, spend, tool errors, duel margins. Needs no key or network.
    Insights {
        /// Recompute the per-log facet cache in logs/facets/
        #[arg(long)]
        refresh: bool,
    },

    /// List live forges registered on this machine
    Ps,

    /// Resume existing forge
    Resume,

    /// Self-update to latest release
    Update,

    /// Restore the workspace from an attempt checkpoint, undoing what a
    /// smith wrote. Bare `slag rewind` takes the most recent attempt.
    Rewind {
        /// Ingot id to rewind; requires --heat
        #[arg(long, value_name = "ID")]
        ingot: Option<String>,
        /// Heat of the attempt to rewind; requires --ingot
        #[arg(long, value_name = "N")]
        heat: Option<u8>,
    },

    /// Session spend beside the OpenRouter account balance
    Cost,

    /// Lifecycle hooks: the event contract and what is configured
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum HooksAction {
    /// Print the events, the exit-code contract, and the configured hooks
    List,
}

/// `slag hooks list`. Reads the frozen snapshot, so it shows what this
/// process would actually fire, not what the file says right now.
pub fn show_hooks() -> Result<(), crate::error::SlagError> {
    print!("{}", crate::engine::hooks::describe(&crate::engine::hooks::snapshot()));
    Ok(())
}

impl Cli {
    pub fn commission_text(&self) -> Option<String> {
        if self.commission.is_empty() {
            None
        } else {
            Some(self.commission.join(" "))
        }
    }

    /// A lone bare word is a mistyped subcommand far more often than it is
    /// a project brief, and the cost of guessing wrong is real: it appends
    /// an addendum to PRD.md and pays for a survey and a founding pass.
    /// `slag finalize` did exactly that. Multi-word commissions -- every
    /// real brief -- are untouched.
    pub fn suspect_verb(&self) -> Option<&str> {
        let [only] = self.commission.as_slice() else { return None };
        let word = only.trim();
        (!word.is_empty() && !word.contains(char::is_whitespace)).then_some(word)
    }
}

/// The subcommands slag answers to, for the message a suspect verb earns.
pub const SUBCOMMANDS: &[&str] =
    &["key", "status", "runs", "insights", "ps", "resume", "update", "rewind", "cost", "hooks"];

/// The closest subcommand to a typo, when one is close enough to name.
/// Two edits covers the ordinary slips -- a dropped letter, a doubled one,
/// a transposition -- without inventing a suggestion for an unrelated word.
pub fn nearest_subcommand(word: &str) -> Option<&'static str> {
    let w = word.to_ascii_lowercase();
    SUBCOMMANDS
        .iter()
        .map(|c| (*c, edit_distance(&w, c)))
        .filter(|(_, d)| *d <= 2)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

// ─── slag status --json (machine-readable contract) ─────────────────────

/// Where the forge persists per-session costs (written by the cost
/// tracker; read-only here). Absent on machines that never forged.
pub const SESSION_COSTS: &str = ".slag/session-costs.json";

/// Build the `slag status --json` object. One JSON object on stdout so
/// external consumers can poll a forge without scraping the TUI. Never
/// demands a key: like `status`, it inspects state that already exists.
pub fn status_json() -> Result<String, SlagError> {
    let crucible_path = Path::new(crate::config::CRUCIBLE);
    let (present, counts) = if crucible_path.exists() {
        (true, crate::crucible::Crucible::load(crucible_path)?.counts())
    } else {
        (false, crate::crucible::CrucibleCounts::default())
    };

    // Newest event log = the live (or last) smith invocation. Its stem
    // doubles as the run id; there is no other run identity on disk.
    let log = newest_log(Path::new(crate::config::LOG_DIR));
    let (mut prompt, mut completion, mut log_cost) = (0u64, 0u64, 0f64);
    let (mut last_event, mut last_prompt) = (None, None);
    if let Some(path) = &log {
        if let Ok(contents) = std::fs::read_to_string(path) {
            prompt = scan_num_sum(&contents, "prompt_tokens") as u64;
            completion = scan_num_sum(&contents, "completion_tokens") as u64;
            log_cost = scan_num_sum(&contents, "cost");
            last_event = scan_str_last(&contents, "event");
            last_prompt = scan_num_last(&contents, "prompt_tokens").map(|n| n as u64);
        }
    }

    // Persisted session costs win over the single-log sum: they cover the
    // whole run, the log only its newest invocation.
    let session_spend = session_costs_spend(Path::new(SESSION_COSTS));
    let spend = session_spend.or((log_cost > 0.0).then_some(log_cost));

    // The context window is model-dependent and not persisted in the
    // logs, so context is reported in tokens; pct stays null unless the
    // cost tracker persisted one.
    let context_pct = std::fs::read_to_string(SESSION_COSTS)
        .ok()
        .and_then(|c| scan_num_last(&c, "context_pct"));

    let run = log
        .as_deref()
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned());

    let obj = serde_json::json!({
        "run": run,
        "crucible": present,
        "ingots": {
            "ore": counts.ore,
            "molten": counts.molten,
            "forged": counts.forged,
            "cracked": counts.cracked,
            "total": counts.total,
        },
        "active_anvils": counts.molten,
        "spend_usd": spend,
        "tokens": {
            "prompt": prompt,
            "completion": completion,
            "total": prompt + completion,
        },
        "context_tokens": last_prompt,
        "context_pct": context_pct,
        "last_event": last_event,
    });
    Ok(obj.to_string())
}

/// `session $0.42 · account $18.31 remaining` (item 36). Either half can
/// be unknown — a run that spent nothing yet, or an unreachable balance
/// endpoint — and the line degrades to whichever half is real rather than
/// printing a zero that reads as fact.
pub fn cost_line(session: Option<f64>, credits: Option<crate::engine::provider::Credits>) -> String {
    let mut parts = Vec::new();
    match session {
        Some(spend) => parts.push(format!("session ${spend:.2}")),
        None => parts.push("session -".to_string()),
    }
    if let Some(c) = credits {
        parts.push(format!("account ${:.2} remaining", c.remaining()));
    }
    parts.join(" · ")
}

/// `slag cost`: this run's spend and the account balance.
pub async fn show_cost() -> Result<(), SlagError> {
    let spend = session_costs_spend(Path::new(SESSION_COSTS));
    // No key: the session half still prints. Reading spend off disk needs
    // no network and no account.
    let credits = match crate::config::EngineConfig::load() {
        Some(cfg) => crate::engine::provider::fetch_credits(&cfg.api_key, &cfg.base_url).await,
        None => None,
    };
    println!("{}", cost_line(spend, credits));
    if let (Some(c), Some(floor)) = (credits, crate::config::credit_floor()) {
        if c.remaining() < floor {
            println!("  balance is under the ${floor:.2} floor — top up before a long run");
        }
    }
    Ok(())
}

/// Pull a spend figure out of `.slag/session-costs.json` without pinning
/// its schema: a bare number, a total-ish key on an object, or a sum of
/// per-entry `cost` fields all work. Returns None when nothing numeric
/// can be found — the caller falls back to the event log.
pub fn session_costs_spend(path: &Path) -> Option<f64> {
    let contents = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    spend_from_value(&value)
}

fn spend_from_value(value: &serde_json::Value) -> Option<f64> {
    if let Some(n) = value.as_f64() {
        return Some(n);
    }
    if let Some(obj) = value.as_object() {
        for key in ["total_usd", "total", "spend_usd", "spend", "cost"] {
            if let Some(v) = obj.get(key) {
                // A present spend key decides this object, even when its
                // value is null (a free-provider CostRecord serializes
                // cost:null): falling through to summing the record's
                // other fields would report token counts as dollars.
                return v.as_f64();
            }
        }
        let sum: f64 = obj.values().filter_map(spend_from_value).sum();
        return (sum > 0.0).then_some(sum);
    }
    if let Some(arr) = value.as_array() {
        let sum: f64 = arr.iter().filter_map(spend_from_value).sum();
        return (sum > 0.0).then_some(sum);
    }
    None
}

/// `logs/run-*.jsonl` is the per-run ledger (RunLog), not an engine event
/// stream: the status/runs surfaces must skip it or a fresh ledger append
/// masquerades as the newest event log and the JSON contract flaps.
fn is_run_ledger(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with("run-"))
}

fn newest_log(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "jsonl") || is_run_ledger(&path) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, p)| p)
}

// ─── No-parse '"key":value' scanner ─────────────────────────────────────
//
// serde-parsing a megabyte-scale event log to answer "what happened last"
// is wasted work; these scanners pull single values straight out of the
// raw text. They rely only on serde_json's compact output shape
// (`"key":value`, no spaces), which is what the sink writes.

/// Last string value for `key`, unescaping the common sequences.
pub fn scan_str_last(hay: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = hay.rfind(&needle)? + needle.len();
    scan_str_at(hay, start)
}

/// First string value for `key` (same unescaping as `scan_str_last`).
pub fn scan_str_first(hay: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = hay.find(&needle)? + needle.len();
    scan_str_at(hay, start)
}

/// Collect the string value starting at `start` (just past the opening
/// quote), through its closing quote. None on a truncated window.
fn scan_str_at(hay: &str, start: usize) -> Option<String> {
    let mut out = String::new();
    let mut chars = hay[start..].chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(c) => out.push(c),
                None => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// Sum of every numeric value for `key` in the window.
pub fn scan_num_sum(hay: &str, key: &str) -> f64 {
    let needle = format!("\"{key}\":");
    hay.match_indices(&needle)
        .filter_map(|(i, _)| parse_num(&hay[i + needle.len()..]))
        .sum()
}

/// Last numeric value for `key` in the window.
pub fn scan_num_last(hay: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\":");
    let start = hay.rfind(&needle)? + needle.len();
    parse_num(&hay[start..])
}

fn parse_num(rest: &str) -> Option<f64> {
    let end = rest
        .find(|c: char| !matches!(c, '0'..='9' | '-' | '+' | '.' | 'e' | 'E'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ─── slag runs (head+tail window listing) ───────────────────────────────

/// Bytes read from each end of a log; a listing must stay instant even
/// with megabyte-scale event logs, so nothing here parses a full file.
pub const RUN_SCAN_WINDOW: u64 = 64 * 1024;

/// One row of the run listing, extracted from stat + two 64KB windows.
#[derive(Debug)]
pub struct RunRow {
    pub name: String,
    pub size: u64,
    pub started: String,
    pub label: String,
    pub last_event: String,
    /// Some(true)=pass, Some(false)=fail, None=in-flight or truncated.
    pub ok: Option<bool>,
}

/// `slag runs` — list past runs, newest first. Reads directory metadata
/// plus head/tail windows only.
pub fn show_runs() -> Result<(), SlagError> {
    let rows = list_runs(Path::new(crate::config::LOG_DIR));
    if rows.is_empty() {
        println!("\n  No runs found. Run `slag \"Your Commission\"` to start.\n");
        return Ok(());
    }

    println!("\n  {}RUNS{} ({}/*.jsonl)\n", fg(tui::HOT), reset(), crate::config::LOG_DIR);
    const MAX_ROWS: usize = 20;
    for row in rows.iter().take(MAX_ROWS) {
        let (glyph, color) = match row.ok {
            Some(true) => ("✓", tui::PURE),
            Some(false) => ("✗", tui::WARM),
            None => ("…", tui::BRIGHT),
        };
        println!(
            "  {}{glyph}{} {}  {}  {:>7}  {}  {}",
            fg(color),
            reset(),
            row.started,
            row.name,
            human_size(row.size),
            tui::truncate(&row.label, 40),
            tui::dim(&row.last_event),
        );
    }
    if rows.len() > MAX_ROWS {
        println!("  {}", tui::dim(&format!("(+{} more)", rows.len() - MAX_ROWS)));
    }
    println!();
    Ok(())
}

/// Collect run rows for every `logs/*.jsonl`, newest first.
pub fn list_runs(dir: &Path) -> Vec<RunRow> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stamped: Vec<(std::time::SystemTime, RunRow)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter(|e| !is_run_ledger(&e.path()))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((modified, run_row(&e.path(), meta.len(), modified)))
        })
        .collect();
    stamped.sort_by(|a, b| b.0.cmp(&a.0));
    stamped.into_iter().map(|(_, row)| row).collect()
}

/// Build one row from stat + head/tail windows. Never parses the file.
fn run_row(path: &Path, size: u64, modified: std::time::SystemTime) -> RunRow {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let (head, tail) = read_windows(path, size);

    // Start time: the sink stamps it into the filename
    // (`engine-YYYYMMDD_HHMMSS`); fall back to mtime for foreign files.
    let started = name
        .rsplit_once('-')
        .and_then(|(_, ts)| {
            chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%d_%H%M%S").ok()
        })
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| {
            chrono::DateTime::<chrono::Local>::from(modified)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        });

    // The events carry no blueprint name; the first ingot's work line is
    // the closest thing to a run label the log offers.
    let label = match (scan_str_first(&head, "id"), scan_str_first(&head, "work")) {
        (Some(id), Some(work)) => format!("[{id}] {work}"),
        (_, Some(work)) => work,
        _ => scan_str_first(&head, "model").unwrap_or_default(),
    };

    let last_event = scan_str_last(&tail, "event").unwrap_or_default();
    RunRow { name, size, started, label, last_event, ok: outcome(&tail) }
}

/// Pass/fail from the tail window: whichever of finish/error appears
/// last wins; neither means the run is still going (or died silently).
fn outcome(tail: &str) -> Option<bool> {
    let finish = tail.rfind("\"event\":\"finish\"");
    let error = tail.rfind("\"event\":\"error\"");
    match (finish, error) {
        (Some(f), Some(e)) => Some(f > e),
        (Some(_), None) => Some(true),
        (None, Some(_)) => Some(false),
        (None, None) => None,
    }
}

/// Head and tail windows of a file, `RUN_SCAN_WINDOW` bytes each, lossy
/// at UTF-8 boundaries (the scanners only need intact key/value spans).
fn read_windows(path: &Path, size: u64) -> (String, String) {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return (String::new(), String::new());
    };
    let mut head = vec![0u8; RUN_SCAN_WINDOW.min(size) as usize];
    if file.read_exact(&mut head).is_err() {
        return (String::new(), String::new());
    }
    let tail = if size > RUN_SCAN_WINDOW {
        let mut tail = vec![0u8; RUN_SCAN_WINDOW as usize];
        match file
            .seek(SeekFrom::End(-(RUN_SCAN_WINDOW as i64)))
            .and_then(|_| file.read_exact(&mut tail))
        {
            Ok(()) => String::from_utf8_lossy(&tail).into_owned(),
            Err(_) => String::new(),
        }
    } else {
        String::from_utf8_lossy(&head).into_owned()
    };
    (String::from_utf8_lossy(&head).into_owned(), tail)
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes}B"),
        1024..=1048575 => format!("{:.1}KB", bytes as f64 / 1024.0),
        _ => format!("{:.1}MB", bytes as f64 / 1048576.0),
    }
}

// ─── PID registry (slag ps + same-crucible guard) ───────────────────────

/// One live forge as registered in the sessions directory.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ForgeSession {
    pub pid: u32,
    pub cwd: String,
    pub run_id: String,
    pub phase: String,
    pub started_at: String,
}

/// Registry home: `$SLAG_SESSIONS_DIR` override (tests), else
/// `~/.slag/sessions`. None only when HOME itself is unset.
pub fn sessions_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SLAG_SESSIONS_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".slag").join("sessions"))
}

/// Is the process still running? `kill -0` reaches processes we own;
/// our own pid short-circuits. A pid we cannot probe counts as dead so
/// a stale entry can never wedge the same-crucible guard forever.
pub fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Register this process in `dir` as `<pid>.json`. Dead entries are
/// pruned on the way in (crash cleanup happens at the next registration,
/// not at the crash). Returns the file to remove on clean exit.
pub fn register_session_in(dir: &Path, run_id: &str, phase: &str) -> Option<PathBuf> {
    prune_dead(dir);
    std::fs::create_dir_all(dir).ok()?;
    let session = ForgeSession {
        pid: std::process::id(),
        cwd: std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .display()
            .to_string(),
        run_id: run_id.to_string(),
        phase: phase.to_string(),
        started_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    let path = dir.join(format!("{}.json", session.pid));
    let body = serde_json::to_string_pretty(&session).ok()?;
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// Live sessions in `dir`, dead pids pruned as a side effect.
pub fn live_sessions_in(dir: &Path) -> Vec<ForgeSession> {
    prune_dead(dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sessions: Vec<ForgeSession> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let contents = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str(&contents).ok()
        })
        .collect();
    sessions.sort_by_key(|s| s.pid);
    sessions
}

fn prune_dead(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let dead = std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str::<ForgeSession>(&c).ok())
            .is_none_or(|s| !pid_alive(s.pid));
        if dead {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Another live forge already lit on `cwd`? Powers the refuse-to-start
/// guard: two forges rewriting one crucible corrupt each other.
pub fn conflict_in(dir: &Path, cwd: &Path) -> Option<ForgeSession> {
    let cwd = cwd.display().to_string();
    live_sessions_in(dir)
        .into_iter()
        .find(|s| s.cwd == cwd && s.pid != std::process::id())
}

/// `slag ps` — list live forges (pid-liveness checked, stale pruned).
pub fn show_ps() -> Result<(), SlagError> {
    let sessions = sessions_dir().map(|d| live_sessions_in(&d)).unwrap_or_default();
    if sessions.is_empty() {
        println!("\n  No live forges.\n");
        return Ok(());
    }
    println!("\n  {}LIVE FORGES{}\n", fg(tui::HOT), reset());
    for s in &sessions {
        println!(
            "  {}{}{}  {}  {}  {}  {}",
            fg(tui::PURE),
            s.pid,
            reset(),
            s.phase,
            s.run_id,
            tui::dim(&s.started_at),
            s.cwd,
        );
    }
    println!();
    Ok(())
}

// ─── slag rewind (item 68's operator handle on the checkpoints) ─────────

/// Restore `root` from an attempt checkpoint and describe what moved.
/// Returns the report rather than printing it, so the outcome is
/// assertable without capturing stdout.
///
/// Bare (both args `None`) means "the most recent attempt", which is the
/// case that matters after a crack: the operator wants the last smith's
/// edits gone. Naming an attempt takes `--ingot` *and* `--heat` together;
/// a half-specified target is refused rather than guessed, because
/// restoring the wrong heat destroys work with no second undo.
pub fn rewind_workspace(
    root: &Path,
    ingot: Option<&str>,
    heat: Option<u8>,
) -> Result<String, SlagError> {
    match (ingot, heat) {
        (Some(id), Some(h)) => {
            let key = crate::anvil::checkpoint::attempt_key(id, h);
            if !crate::anvil::checkpoint::attempt_exists(root, id, h) {
                return Ok(format!("no checkpoint for {key}"));
            }
            let restored = crate::anvil::checkpoint::rewind_attempt(root, id, h);
            Ok(format!("rewound {key}: {restored} file(s) restored"))
        }
        (None, None) => match crate::anvil::checkpoint::rewind_latest(root) {
            Some((key, restored)) => {
                Ok(format!("rewound {key}: {restored} file(s) restored"))
            }
            None => Ok("no checkpoint to rewind".into()),
        },
        _ => Err(SlagError::Config(
            "slag rewind needs --ingot and --heat together, or neither".into(),
        )),
    }
}

/// `slag rewind` — print what the rewind restored.
pub fn show_rewind(ingot: Option<&str>, heat: Option<u8>) -> Result<(), SlagError> {
    let report = rewind_workspace(Path::new("."), ingot, heat)?;
    println!("\n  {}⟲{} {report}\n", fg(tui::HOT), reset());
    Ok(())
}

#[cfg(test)]
mod rewind_tests {
    use super::*;
    use crate::anvil::checkpoint::Checkpoint;

    /// Stage one attempt that modified `file`, so rewind has something to
    /// restore. Returns the pre-modification content.
    fn staged_attempt(root: &Path, id: &str, heat: u8, file: &str) -> &'static str {
        let path = root.join(file);
        std::fs::write(&path, b"original\n").unwrap();
        let cp = Checkpoint::for_attempt(root, id, heat);
        cp.begin();
        cp.record(&path);
        std::fs::write(&path, b"smith wrote this\n").unwrap();
        "original\n"
    }

    /// Item 68: bare `slag rewind` restores the most recent attempt and
    /// names what it undid, so the operator can see the damage reversed.
    #[test]
    fn rewind_restores_latest_attempt_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let want = staged_attempt(dir.path(), "i1", 2, "src.txt");

        let report = rewind_workspace(dir.path(), None, None).unwrap();

        assert_eq!(std::fs::read_to_string(dir.path().join("src.txt")).unwrap(), want);
        assert!(report.contains("i1-h2"), "{report}");
        assert!(report.contains('1'), "{report}");
    }

    /// `--ingot`/`--heat` target one attempt by name rather than by mtime.
    #[test]
    fn rewind_targets_a_named_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let want = staged_attempt(dir.path(), "i1", 1, "a.txt");
        staged_attempt(dir.path(), "i9", 4, "b.txt");

        let report = rewind_workspace(dir.path(), Some("i1"), Some(1)).unwrap();

        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), want);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "smith wrote this\n",
            "the untargeted attempt must stay wound forward"
        );
        assert!(report.contains("i1-h1"), "{report}");
    }

    /// Nothing to rewind is a normal outcome, not an error: a workspace
    /// that never forged has no manifests, and `slag rewind` must say so
    /// rather than fail the exit code.
    #[test]
    fn rewind_reports_when_no_checkpoint_exists() {
        let dir = tempfile::tempdir().unwrap();
        let report = rewind_workspace(dir.path(), None, None).unwrap();
        assert!(report.to_lowercase().contains("no checkpoint"), "{report}");
    }

    /// A mistyped id must not read as success. "rewound i9-h1: 0 file(s)
    /// restored" tells an operator their rewind ran; it did not, and the
    /// files they wanted back are still forward.
    #[test]
    fn rewind_names_a_target_that_was_never_checkpointed() {
        let dir = tempfile::tempdir().unwrap();
        staged_attempt(dir.path(), "i1", 1, "a.txt");

        let report = rewind_workspace(dir.path(), Some("i9"), Some(1)).unwrap();

        assert!(report.to_lowercase().contains("no checkpoint"), "{report}");
        assert!(report.contains("i9-h1"), "{report}");
        assert!(!report.contains("rewound"), "{report}");
    }

    /// An `--ingot` with no `--heat` is ambiguous; refuse rather than
    /// guess a heat and silently restore the wrong attempt.
    #[test]
    fn rewind_refuses_a_half_specified_target() {
        let dir = tempfile::tempdir().unwrap();
        assert!(rewind_workspace(dir.path(), Some("i1"), None).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Item 36: session spend beside the account balance, degrading to
    /// whichever half is real rather than printing a zero as fact.
    #[test]
    fn cost_line_shows_both_halves_and_survives_either_being_unknown() {
        let credits = crate::engine::provider::Credits { granted: 20.0, used: 1.69 };
        assert_eq!(
            cost_line(Some(0.42), Some(credits)),
            "session $0.42 · account $18.31 remaining"
        );
        // Unreachable balance endpoint: the session half still prints.
        assert_eq!(cost_line(Some(0.42), None), "session $0.42");
        // Nothing spent yet reads as unknown, not as $0.00.
        assert_eq!(
            cost_line(None, Some(credits)),
            "session - · account $18.31 remaining"
        );
        assert_eq!(cost_line(None, None), "session -");
    }

    #[test]
    fn cost_subcommand_parses() {
        assert!(matches!(
            Cli::parse_from(["slag", "cost"]).command,
            Some(Command::Cost)
        ));
    }

    #[test]
    fn duel_flag_parses_and_defaults_off() {
        let cli = Cli::parse_from(["slag", "--duel", "build", "it"]);
        assert!(cli.duel);
        assert_eq!(cli.commission_text().as_deref(), Some("build it"));

        let cli = Cli::parse_from(["slag", "build", "it"]);
        assert!(!cli.duel);
    }

    #[test]
    fn trace_flag_takes_a_path_and_stays_off_by_default() {
        let cli = Cli::parse_from(["slag", "--trace", "run.json", "build", "it"]);
        assert_eq!(cli.trace.as_deref(), Some(Path::new("run.json")));
        assert_eq!(cli.commission_text().as_deref(), Some("build it"));

        let cli = Cli::parse_from(["slag", "build", "it"]);
        assert!(cli.trace.is_none(), "tracing costs a file; opt in explicitly");
    }

    #[test]
    fn trace_flag_works_on_resume_too() {
        let cli = Cli::parse_from(["slag", "--trace", "run.json", "resume"]);
        assert_eq!(cli.trace.as_deref(), Some(Path::new("run.json")));
        assert!(matches!(cli.command, Some(Command::Resume)));
    }

    #[test]
    fn worktree_flag_still_parses_and_help_says_it_is_ignored() {
        let cli = Cli::parse_from(["slag", "--worktree", "build", "it"]);
        assert!(cli.worktree);
        assert_eq!(cli.commission_text().as_deref(), Some("build it"));

        // The advertised behavior must match reality: the help text says
        // the flag is ignored until the isolation is actually wired up.
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("not implemented yet"), "help: {help}");
    }

    #[test]
    fn status_json_runs_and_ps_parse() {
        let cli = Cli::parse_from(["slag", "status", "--json"]);
        assert!(matches!(cli.command, Some(Command::Status { json: true })));

        let cli = Cli::parse_from(["slag", "status"]);
        assert!(matches!(cli.command, Some(Command::Status { json: false })));

        assert!(matches!(Cli::parse_from(["slag", "runs"]).command, Some(Command::Runs)));
        assert!(matches!(Cli::parse_from(["slag", "ps"]).command, Some(Command::Ps)));
    }

    // ── scanner ──

    #[test]
    fn scanner_pulls_values_without_parsing() {
        let log = concat!(
            r#"{"event":"turn_start","turn":1}"#, "\n",
            r#"{"event":"tokens","usage":{"prompt_tokens":100,"completion_tokens":20,"cost":0.5}}"#, "\n",
            r#"{"event":"tokens","usage":{"prompt_tokens":200,"completion_tokens":30,"cost":0.25}}"#, "\n",
            r#"{"event":"finish","summary":"done \"quoted\" work"}"#, "\n",
        );
        assert_eq!(scan_num_sum(log, "prompt_tokens"), 300.0);
        assert_eq!(scan_num_sum(log, "completion_tokens"), 50.0);
        assert_eq!(scan_num_sum(log, "cost"), 0.75);
        assert_eq!(scan_num_last(log, "prompt_tokens"), Some(200.0));
        assert_eq!(scan_str_last(log, "event").as_deref(), Some("finish"));
        assert_eq!(scan_str_first(log, "event").as_deref(), Some("turn_start"));
        // Escaped quotes inside a value must not truncate it.
        assert_eq!(scan_str_last(log, "summary").as_deref(), Some(r#"done "quoted" work"#));
        assert_eq!(scan_str_last(log, "missing"), None);
        assert_eq!(scan_num_last(log, "missing"), None);
    }

    #[test]
    fn outcome_reads_the_last_terminal_event() {
        assert_eq!(outcome(r#"{"event":"finish","summary":"ok"}"#), Some(true));
        assert_eq!(outcome(r#"{"event":"error","message":"boom"}"#), Some(false));
        // A retry error followed by a finish is a pass.
        assert_eq!(
            outcome(concat!(
                r#"{"event":"error","message":"transient"}"#,
                r#"{"event":"finish","summary":"ok"}"#,
            )),
            Some(true)
        );
        assert_eq!(outcome(r#"{"event":"tool_result","name":"bash"}"#), None);
    }

    // ── session costs ──

    #[test]
    fn session_costs_accepts_several_schemas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-costs.json");

        std::fs::write(&path, "1.25").unwrap();
        assert_eq!(session_costs_spend(&path), Some(1.25));

        std::fs::write(&path, r#"{"total_usd": 2.5, "runs": 3}"#).unwrap();
        assert_eq!(session_costs_spend(&path), Some(2.5));

        std::fs::write(&path, r#"[{"cost": 1.0}, {"cost": 0.5}]"#).unwrap();
        assert_eq!(session_costs_spend(&path), Some(1.5));

        std::fs::write(&path, r#"{"note": "no numbers"}"#).unwrap();
        assert_eq!(session_costs_spend(&path), None);

        assert_eq!(session_costs_spend(&dir.path().join("absent.json")), None);
    }

    /// The real session-costs.json shape: a map of run-id → CostRecord.
    /// A free-provider record (cost:null) must never fall through to
    /// summing its token counts as dollars.
    #[test]
    fn session_costs_never_reports_token_counts_as_dollars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-costs.json");

        std::fs::write(
            &path,
            r#"{"a1b2c3d4e5f60718": {"prompt_tokens": 100000, "completion_tokens": 18234,
                "total_tokens": 118234, "cost": null}}"#,
        )
        .unwrap();
        assert_eq!(session_costs_spend(&path), None, "free run: no spend, not 236k dollars");

        // A paid record alongside a free one reports only the real cost.
        std::fs::write(
            &path,
            r#"{"free00free00free": {"prompt_tokens": 5000, "total_tokens": 6000, "cost": null},
                "paid00paid00paid": {"prompt_tokens": 10, "total_tokens": 20, "cost": 0.5}}"#,
        )
        .unwrap();
        assert_eq!(session_costs_spend(&path), Some(0.5));
    }

    // ── runs ──

    #[test]
    fn list_runs_reads_windows_not_whole_files() {
        let dir = tempfile::tempdir().unwrap();

        // A log bigger than one window whose verdict lives in the tail.
        let mut big = String::from(
            "{\"event\":\"ingot_start\",\"id\":\"i7\",\"work\":\"wire the lever\"}\n",
        );
        let filler = format!("{{\"event\":\"narrate\",\"text\":\"{}\"}}\n", "x".repeat(200));
        while (big.len() as u64) < RUN_SCAN_WINDOW + 4096 {
            big.push_str(&filler);
        }
        big.push_str("{\"event\":\"finish\",\"summary\":\"ok\"}\n");
        std::fs::write(dir.path().join("engine-20260818_101112.jsonl"), &big).unwrap();
        std::fs::write(
            dir.path().join("engine-20260818_090000.jsonl"),
            "{\"event\":\"error\",\"message\":\"boom\"}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a log").unwrap();

        let rows = list_runs(dir.path());
        assert_eq!(rows.len(), 2, "non-jsonl files must not list");

        let big_row = rows.iter().find(|r| r.name.contains("101112")).unwrap();
        assert_eq!(big_row.ok, Some(true), "finish in tail window: {big_row:?}");
        assert_eq!(big_row.label, "[i7] wire the lever");
        assert_eq!(big_row.started, "2026-08-18 10:11");
        assert_eq!(big_row.last_event, "finish");

        let small = rows.iter().find(|r| r.name.contains("090000")).unwrap();
        assert_eq!(small.ok, Some(false));
        assert_eq!(small.started, "2026-08-18 09:00");
    }

    #[test]
    fn list_runs_on_missing_dir_is_empty() {
        assert!(list_runs(Path::new("/definitely/not/a/dir")).is_empty());
    }

    /// run-*.jsonl is the RunLog ledger, not an event stream: `slag runs`
    /// must not double-list it, and `status --json` must not treat a
    /// fresh ledger append as the newest event log.
    #[test]
    fn run_ledgers_are_excluded_from_runs_and_newest_log() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine-20260818_101112.jsonl");
        std::fs::write(&engine, "{\"event\":\"finish\",\"summary\":\"ok\"}\n").unwrap();
        // The ledger is written after the engine log (newer mtime).
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            dir.path().join("run-20260818_101112-424242.jsonl"),
            "{\"entry\":\"run_meta\",\"run_id\":\"20260818_101112-424242\"}\n",
        )
        .unwrap();

        let rows = list_runs(dir.path());
        assert_eq!(rows.len(), 1, "the ledger must not list as a phantom run");
        assert!(rows[0].name.starts_with("engine-"));

        let newest = newest_log(dir.path()).expect("engine log found");
        assert_eq!(newest, engine, "the newer ledger must not win");
    }

    #[test]
    fn human_size_buckets() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(2048), "2.0KB");
        assert_eq!(human_size(3 * 1048576), "3.0MB");
    }

    // ── pid registry ──

    #[test]
    fn register_conflict_and_prune_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();

        // Register ourselves: we are live, and we never conflict with
        // our own registration.
        let path = register_session_in(dir.path(), "run-1", "forge").unwrap();
        assert!(path.exists());
        let sessions = live_sessions_in(dir.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].run_id, "run-1");
        assert_eq!(sessions[0].phase, "forge");
        assert!(conflict_in(dir.path(), &cwd).is_none(), "own pid is not a conflict");

        // A dead pid on the same cwd prunes instead of blocking. u32::MAX
        // is not a valid live pid anywhere.
        let ghost = ForgeSession {
            pid: u32::MAX - 1,
            cwd: cwd.display().to_string(),
            run_id: "run-0".into(),
            phase: "forge".into(),
            started_at: "2026-08-18 09:00:00".into(),
        };
        let ghost_path = dir.path().join(format!("{}.json", ghost.pid));
        std::fs::write(&ghost_path, serde_json::to_string(&ghost).unwrap()).unwrap();
        assert!(conflict_in(dir.path(), &cwd).is_none(), "dead pid must not block");
        assert!(!ghost_path.exists(), "dead entry should be pruned");

        // Corrupt entries prune too instead of poisoning every listing.
        let junk = dir.path().join("junk.json");
        std::fs::write(&junk, "not json").unwrap();
        assert_eq!(live_sessions_in(dir.path()).len(), 1);
        assert!(!junk.exists());
    }

    #[test]
    fn own_pid_is_alive_and_garbage_pid_is_not() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX - 1));
    }
}

#[cfg(test)]
mod verb_tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("slag").chain(args.iter().copied()))
    }

    #[test]
    fn a_lone_bare_word_is_treated_as_a_mistyped_subcommand() {
        // `slag finalize` appended an addendum to PRD.md and paid for a
        // survey, because a trailing var-arg swallows anything clap cannot
        // match to a subcommand.
        assert_eq!(parse(&["finalize"]).suspect_verb(), Some("finalize"));
    }

    #[test]
    fn a_real_commission_is_never_suspect() {
        assert_eq!(parse(&["build a shooter in three.js"]).suspect_verb(), None);
        assert_eq!(parse(&["build", "a", "shooter"]).suspect_verb(), None);
        assert_eq!(parse(&[]).suspect_verb(), None);
    }

    #[test]
    fn force_is_the_way_to_commission_a_one_word_project() {
        // The flag has to precede the word: `commission` is a trailing
        // var-arg, so anything after the first positional is swallowed
        // into the brief rather than parsed as a flag.
        let cli = parse(&["--force", "tetris"]);
        assert!(cli.force);
        assert_eq!(cli.commission_text().as_deref(), Some("tetris"));
        assert_eq!(parse(&["tetris", "--force"]).commission_text().as_deref(), Some("tetris --force"));
    }

    #[test]
    fn a_near_miss_earns_a_suggestion() {
        assert_eq!(nearest_subcommand("insight"), Some("insights"));
        assert_eq!(nearest_subcommand("statu"), Some("status"));
        assert_eq!(nearest_subcommand("updat"), Some("update"));
    }

    #[test]
    fn an_unrelated_word_gets_no_invented_suggestion() {
        assert_eq!(nearest_subcommand("finalize"), None);
        assert_eq!(nearest_subcommand("shooter"), None);
    }
}
