//! tape — one run on one timeline, recorded so `slag replay` can play it.
//!
//! A forge already logs its events, but per smith and without a clock:
//! each `logs/engine-*.jsonl` holds one agent session, and the pipeline's
//! own events (ingot start and finish, heats, the warden's word) reach no
//! file at all in stream mode. A tape is the whole run on one timeline:
//! every engine event with its offset in milliseconds, the stream marks
//! the pipeline printed through `tui` (banner, phase headers, status
//! lines), and a copy of PLAN.md each time it changed.
//!
//! One JSON object per line:
//!
//! ```text
//! {"tape":1,"version":"2.9.0","started":"2026-10-15T10:00:00","cols":120,"rows":36}
//! {"t":0,"mark":"banner"}
//! {"t":12,"mark":"header","text":"SURVEYOR · deep analysis"}
//! {"t":840,"event":"turn_start","turn":1}
//! {"t":9120,"plan":"(ingot :id \"i1\" ...)"}
//! ```
//!
//! An event line is an `engine-*.jsonl` line plus `t`, so the reader takes
//! either kind of file. An untimed log plays on a synthetic clock.
//!
//! The reader half is pure: it turns text into `Timed` cues and finds the
//! named journey points (`locate`) that the screenshot script stops on.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::engine::{EngineEvent, Usage};

/// Tape format version, written into the header line.
pub const TAPE_VERSION: u32 = 1;

/// A palette slot by name, so a mark survives a palette change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Cold,
    Warm,
    Hot,
    Bright,
    Pure,
}

impl Tone {
    pub fn of(color: crossterm::style::Color) -> Self {
        match color {
            c if c == crate::tui::WARM => Tone::Warm,
            c if c == crate::tui::HOT => Tone::Hot,
            c if c == crate::tui::BRIGHT => Tone::Bright,
            c if c == crate::tui::PURE => Tone::Pure,
            _ => Tone::Cold,
        }
    }

    pub fn color(self) -> crossterm::style::Color {
        match self {
            Tone::Cold => crate::tui::COLD,
            Tone::Warm => crate::tui::WARM,
            Tone::Hot => crate::tui::HOT,
            Tone::Bright => crate::tui::BRIGHT,
            Tone::Pure => crate::tui::PURE,
        }
    }
}

/// Crucible counts as a mark carries them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub total: usize,
    pub ore: usize,
    pub molten: usize,
    pub forged: usize,
    pub cracked: usize,
}

impl Tally {
    pub fn of(c: &crate::crucible::CrucibleCounts) -> Self {
        Self { total: c.total, ore: c.ore, molten: c.molten, forged: c.forged, cracked: c.cracked }
    }

    pub fn counts(self) -> crate::crucible::CrucibleCounts {
        crate::crucible::CrucibleCounts {
            total: self.total,
            ore: self.ore,
            molten: self.molten,
            forged: self.forged,
            cracked: self.cracked,
        }
    }
}

/// Something the pipeline printed through `tui`, recorded so a replay can
/// print it again through the same function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mark", rename_all = "snake_case")]
pub enum Mark {
    Banner,
    Header { text: String },
    Status { icon: String, tone: Tone, text: String },
    Counts { tally: Tally },
    TemperBar { tally: Tally },
}

/// How one heat ended: the command the smith offered as proof, whether
/// it held, and the first line of why not. Read from the forge's own
/// STRIKE/ASSAY logs the moment the heat resolves, since no event says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub id: String,
    pub heat: u8,
    #[serde(default)]
    pub cmd: Option<String>,
    pub ok: bool,
    #[serde(default)]
    pub why: Option<String>,
}

/// The run-wide figures the assay reports, captured when the run closes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    #[serde(default)]
    pub durations: Option<String>,
    #[serde(default)]
    pub churn: Option<String>,
    #[serde(default)]
    pub tool_errors: Option<String>,
    #[serde(default)]
    pub spend: Option<String>,
    #[serde(default)]
    pub ledger: Vec<String>,
    /// `git diff --shortstat` from the commit the run started on to HEAD:
    /// what the run changed, as git counts it.
    #[serde(default)]
    pub diff: Option<String>,
    /// Commits the run added, and where the work now is.
    #[serde(default)]
    pub commits: Option<usize>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

/// HEAD when the run began, so the assay can say what the run changed.
static START_HEAD: Mutex<Option<String>> = Mutex::new(None);

fn git(args: &[&str]) -> Option<String> {
    let out = crate::engine::sandbox::git().args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

impl Stats {
    /// Note where the run starts. Outside a git repository there is
    /// nothing to note, and the assay leaves the diff out.
    pub fn mark_start() {
        *START_HEAD.lock().unwrap_or_else(|p| p.into_inner()) = git(&["rev-parse", "HEAD"]);
    }

    /// What `engine::stats` holds right now, and what git says changed.
    pub fn capture() -> Self {
        use crate::engine::stats;
        let run = stats::snapshot();
        let total = run.ledger.total();
        let start = START_HEAD.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let range = start.as_ref().map(|s| format!("{s}..HEAD"));
        Self {
            durations: stats::durations_line(&run),
            churn: stats::churn_line(&run),
            tool_errors: stats::tool_errors_line(&run),
            spend: (total.total_tokens > 0 || total.cost.is_some())
                .then(|| crate::engine::pricing::format_cost(&total)),
            ledger: stats::ledger_lines(&run).unwrap_or_default(),
            diff: start.as_deref().and_then(|s| git(&["diff", "--shortstat", s, "HEAD"])),
            commits: range
                .as_deref()
                .and_then(|r| git(&["rev-list", "--count", r]))
                .and_then(|n| n.parse().ok()),
            head: start.as_ref().and(git(&["rev-parse", "--short", "HEAD"])),
            branch: start.as_ref().and(git(&["rev-parse", "--abbrev-ref", "HEAD"])),
        }
    }
}

/// One thing that happened, without its time.
#[derive(Debug, Clone)]
pub enum Cue {
    Event(EngineEvent),
    Mark(Mark),
    /// PLAN.md as it stood at this moment.
    Plan(String),
    /// A document the pipeline showed an excerpt of (the blueprint), or
    /// the commission (PRD.md).
    Doc { name: String, text: String },
    /// A heat's outcome, just before the event that ended it.
    Proof(Proof),
    /// The assay's figures, as the run closed.
    Stats(Stats),
}

/// A cue and its offset from the start of the run.
#[derive(Debug, Clone)]
pub struct Timed {
    pub t: u64,
    pub cue: Cue,
}

/// The tape's first line.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub tape: u32,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub started: String,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
    /// The command line that started the run, flags and commission.
    #[serde(default)]
    pub argv: Vec<String>,
}

/// A whole tape, read back.
#[derive(Debug, Clone, Default)]
pub struct Tape {
    pub meta: Option<Meta>,
    pub cues: Vec<Timed>,
    /// Lines that did not parse. A replay still plays; the count is shown.
    pub skipped: usize,
}

impl Tape {
    /// Offset of the last cue: the run's length.
    pub fn duration(&self) -> u64 {
        self.cues.last().map_or(0, |c| c.t)
    }

}

// ─── reading ────────────────────────────────────────────────────────────

pub fn read(path: &Path) -> Result<Tape, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(parse(&text))
}

/// Parse a tape, or an untimed `engine-*.jsonl` log. Unparseable lines are
/// counted and skipped: one torn line at the end of a crashed run must not
/// cost the whole recording.
pub fn parse(text: &str) -> Tape {
    let mut tape = Tape::default();
    let mut clock: u64 = 0;
    let mut prev: Option<Cue> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            tape.skipped += 1;
            continue;
        };
        if v.get("tape").is_some() {
            if let Ok(meta) = serde_json::from_value::<Meta>(v.clone()) {
                tape.meta = Some(meta);
                continue;
            }
        }
        let Some(cue) = decode_cue(&v) else {
            tape.skipped += 1;
            continue;
        };
        let t = match v.get("t").and_then(Value::as_u64) {
            Some(t) => t.max(clock),
            None => clock + prev.as_ref().map_or(0, synth_gap),
        };
        clock = t;
        prev = Some(cue.clone());
        tape.cues.push(Timed { t, cue });
    }
    tape
}

/// Time an untimed log spends after each kind of cue. Rough, but in the
/// right order of magnitude, so a replayed engine log reads like a run
/// rather than a dump: models think, tools are quick.
fn synth_gap(cue: &Cue) -> u64 {
    match cue {
        Cue::Event(ev) => match ev {
            EngineEvent::TurnStart { .. } => 300,
            EngineEvent::ModelCall { .. } => 1400,
            EngineEvent::ToolCallStart { .. } => 500,
            EngineEvent::ToolResult { .. } => 150,
            EngineEvent::Tokens { .. } => 100,
            EngineEvent::Narrate { .. } => 200,
            EngineEvent::Finish { .. } => 400,
            _ => 120,
        },
        _ => 50,
    }
}

fn decode_cue(v: &Value) -> Option<Cue> {
    if v.get("event").is_some() {
        return decode_event(v).map(Cue::Event);
    }
    if v.get("mark").is_some() {
        return serde_json::from_value::<Mark>(v.clone()).ok().map(Cue::Mark);
    }
    if let Some(plan) = v.get("plan").and_then(Value::as_str) {
        return Some(Cue::Plan(plan.to_string()));
    }
    if let (Some(name), Some(text)) =
        (v.get("doc").and_then(Value::as_str), v.get("text").and_then(Value::as_str))
    {
        return Some(Cue::Doc { name: name.to_string(), text: text.to_string() });
    }
    if let Some(p) = v.get("proof") {
        return serde_json::from_value::<Proof>(p.clone()).ok().map(Cue::Proof);
    }
    if let Some(st) = v.get("stats") {
        return serde_json::from_value::<Stats>(st.clone()).ok().map(Cue::Stats);
    }
    None
}

/// `EngineEvent` only serializes, and its shape belongs to the engine, so
/// the reader decodes by hand. Missing fields default the way the engine's
/// own serde defaults would, so a log written by an older slag still plays.
pub fn decode_event(v: &Value) -> Option<EngineEvent> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let ev = match v.get("event")?.as_str()? {
        "turn_start" => EngineEvent::TurnStart { turn: n("turn") as usize },
        "model_call" => EngineEvent::ModelCall { model: s("model") },
        "model_routed" => EngineEvent::ModelRouted { requested: s("requested"), routed: s("routed") },
        "tool_call_start" => EngineEvent::ToolCallStart { name: s("name"), preview: s("preview") },
        "tool_result" => EngineEvent::ToolResult {
            name: s("name"),
            ok: b("ok"),
            preview: s("preview"),
            lines: n("lines") as usize,
            bytes: n("bytes") as usize,
            ms: n("ms"),
        },
        "tokens" => EngineEvent::Tokens {
            usage: v
                .get("usage")
                .and_then(|u| serde_json::from_value::<Usage>(u.clone()).ok())
                .unwrap_or_default(),
        },
        "steer" => EngineEvent::Steer { text: s("text") },
        "finish" => EngineEvent::Finish { summary: s("summary") },
        "error" => EngineEvent::Error { message: s("message") },
        "narrate" => EngineEvent::Narrate { text: s("text") },
        "warning" => EngineEvent::Warning { message: s("message") },
        "api_retry" => EngineEvent::ApiRetry {
            attempt: n("attempt") as usize,
            status: n("status") as u16,
            remaining_secs: n("remaining_secs"),
        },
        "ingot_start" => EngineEvent::IngotStart { id: s("id"), work: s("work") },
        "heat_tick" => EngineEvent::HeatTick { id: s("id"), heat: n("heat") as u8 },
        "ingot_done" => EngineEvent::IngotDone { id: s("id"), ok: b("ok") },
        "duel_round" => EngineEvent::DuelRound { id: s("id"), round: n("round") as u8 },
        "duel_verdict" => EngineEvent::DuelVerdict {
            id: s("id"),
            winner: s("winner").chars().next().unwrap_or('a'),
            margin: n("margin") as u8,
        },
        "context_gauge" => EngineEvent::ContextGauge {
            pct: n("pct") as u8,
            used_tokens: n("used_tokens"),
            budget_tokens: n("budget_tokens") as usize,
        },
        "hook_started" => EngineEvent::HookStarted {
            name: s("name"),
            hook_event: s("hook_event"),
            status_message: v.get("status_message").and_then(Value::as_str).map(str::to_string),
        },
        "hook_finished" => EngineEvent::HookFinished {
            name: s("name"),
            hook_event: s("hook_event"),
            code: v.get("code").and_then(Value::as_i64).unwrap_or(0) as i32,
            duration_ms: n("duration_ms"),
        },
        _ => return None,
    };
    Some(ev)
}

/// One cue as a tape line.
pub fn encode(t: u64, cue: &Cue) -> Option<String> {
    let mut v = match cue {
        Cue::Event(ev) => serde_json::to_value(ev).ok()?,
        Cue::Mark(m) => serde_json::to_value(m).ok()?,
        Cue::Plan(p) => serde_json::json!({ "plan": p }),
        Cue::Doc { name, text } => serde_json::json!({ "doc": name, "text": text }),
        Cue::Proof(p) => serde_json::json!({ "proof": p }),
        Cue::Stats(st) => serde_json::json!({ "stats": st }),
    };
    let obj = v.as_object_mut()?;
    // `t` first reads better in a pager; serde_json keeps insertion order
    // only with a feature slag does not enable, so build the line by hand.
    let body = serde_json::to_string(obj).ok()?;
    Some(format!("{{\"t\":{t},{}", &body[1..]))
}

// ─── journey points ─────────────────────────────────────────────────────

/// Named moments a screenshot or a critic wants to stop on. Each resolves
/// to an offset into the tape.
pub const POINTS: &[(&str, &str)] = &[
    ("start", "the banner and the first survey turn"),
    ("survey", "the surveyor at work"),
    ("plan", "the founder's mold, just cast"),
    ("forge", "the first ingot on the anvil"),
    ("anvils", "two or more ingots forging at once"),
    ("retry", "an ingot back on the anvil for another heat"),
    ("crack", "an ingot that failed for good, or a failed proof"),
    ("warden", "the warden's verdict on the goal"),
    ("assay", "the final report"),
];

/// Resolve `spec` to an offset: a named point, `12.5s`, `840ms`, `40%`, or
/// a bare number of milliseconds.
pub fn locate(tape: &Tape, spec: &str) -> Result<u64, String> {
    let spec = spec.trim();
    let end = tape.duration();
    if let Some(pct) = spec.strip_suffix('%') {
        let pct: f64 = pct.parse().map_err(|_| format!("bad percentage `{spec}`"))?;
        return Ok(((end as f64) * pct.clamp(0.0, 100.0) / 100.0).round() as u64);
    }
    if let Some(ms) = spec.strip_suffix("ms") {
        return ms.parse().map_err(|_| format!("bad offset `{spec}`"));
    }
    if let Some(s) = spec.strip_suffix('s') {
        if let Ok(secs) = s.parse::<f64>() {
            return Ok((secs * 1000.0).round() as u64);
        }
    }
    if let Ok(ms) = spec.parse::<u64>() {
        return Ok(ms);
    }
    let found = match spec {
        "start" => nth_after(tape, |c| is_event(c, |e| matches!(e, EngineEvent::TurnStart { .. })), 3),
        "survey" => midpoint_of_phase(tape, "SURVEYOR"),
        "plan" => tape.cues.iter().position(|c| {
            matches!(&c.cue, Cue::Mark(Mark::Status { text, .. }) if text.starts_with("Mold"))
        }),
        "forge" => nth_after(
            tape,
            |c| is_event(c, |e| matches!(e, EngineEvent::IngotStart { .. })),
            4,
        ),
        "anvils" => anvils_point(tape),
        "retry" => tape.cues.iter().position(|c| {
            is_event(c, |e| matches!(e, EngineEvent::HeatTick { heat, .. } if *heat >= 2))
        }).map(|i| advance(tape, i, 3)),
        "crack" => tape
            .cues
            .iter()
            .position(|c| is_event(c, |e| matches!(e, EngineEvent::IngotDone { ok: false, .. })))
            .or_else(|| {
                tape.cues.iter().position(|c| {
                    is_event(c, |e| matches!(e, EngineEvent::ToolResult { ok: false, .. }))
                })
            })
            .map(|i| advance(tape, i, 1)),
        // The goal verdict when the run reached one; otherwise the first
        // time the warden sent a plan document back.
        "warden" => warden_at(tape, "warden round")
            .or_else(|| warden_at(tape, "warden rejected the"))
            .map(|i| advance(tape, i, 1)),
        "assay" | "end" => return Ok(end),
        other => {
            let names: Vec<&str> = POINTS.iter().map(|(n, _)| *n).collect();
            return Err(format!(
                "unknown point `{other}`; use one of {}, or an offset like 12s or 40%",
                names.join(", ")
            ));
        }
    };
    match found {
        Some(i) => Ok(tape.cues[i].t),
        None => Err(format!("this tape has no `{spec}` moment")),
    }
}

fn warden_at(tape: &Tape, prefix: &str) -> Option<usize> {
    tape.cues.iter().position(|c| {
        is_event(c, |e| match e {
            EngineEvent::Warning { message } | EngineEvent::Narrate { text: message } => {
                message.starts_with(prefix)
            }
            _ => false,
        })
    })
}

fn is_event(c: &Timed, f: impl Fn(&EngineEvent) -> bool) -> bool {
    matches!(&c.cue, Cue::Event(e) if f(e))
}

/// Index a few cues past the first match, so the frame shows what the
/// moment led to rather than the bare moment itself.
fn nth_after(tape: &Tape, f: impl Fn(&Timed) -> bool, ahead: usize) -> Option<usize> {
    tape.cues.iter().position(f).map(|i| advance(tape, i, ahead))
}

/// `ahead` cues later, counting only events that draw something.
fn advance(tape: &Tape, from: usize, ahead: usize) -> usize {
    let mut seen = 0;
    for (i, c) in tape.cues.iter().enumerate().skip(from + 1) {
        if let Cue::Event(e) = &c.cue {
            if matches!(e, EngineEvent::Tokens { .. } | EngineEvent::ContextGauge { .. }) {
                continue;
            }
            seen += 1;
            if seen >= ahead {
                return i;
            }
        }
    }
    from
}

/// Halfway through the cues between `header`'s mark and the next header.
fn midpoint_of_phase(tape: &Tape, header: &str) -> Option<usize> {
    let start = tape.cues.iter().position(
        |c| matches!(&c.cue, Cue::Mark(Mark::Header { text }) if text.starts_with(header)),
    )?;
    let end = tape.cues[start + 1..]
        .iter()
        .position(|c| matches!(&c.cue, Cue::Mark(Mark::Header { .. })))
        .map_or(tape.cues.len() - 1, |i| start + i);
    Some(start + (end - start) / 2)
}

/// The busiest moment: the cue where the most ingots are open at once,
/// a few events into it so every anvil has shown some work.
fn anvils_point(tape: &Tape) -> Option<usize> {
    let mut open: Vec<&str> = Vec::new();
    let mut best: Option<(usize, usize)> = None;
    for (i, c) in tape.cues.iter().enumerate() {
        match &c.cue {
            Cue::Event(EngineEvent::IngotStart { id, .. }) => {
                if !open.contains(&id.as_str()) {
                    open.push(id);
                }
            }
            Cue::Event(EngineEvent::IngotDone { id, .. }) => open.retain(|o| o != id),
            _ => {}
        }
        if open.len() >= 2 && best.is_none_or(|(n, _)| open.len() > n) {
            best = Some((open.len(), i));
        }
    }
    best.map(|(_, i)| advance(tape, i, 6))
}

// ─── recording ──────────────────────────────────────────────────────────

type Stamped = (Instant, Cue);

/// The open session's inbox. `tui` marks reach it from any thread through
/// `mark`; a closed or absent session makes marking a no-op.
fn bus() -> &'static Mutex<Option<UnboundedSender<Stamped>>> {
    static BUS: Mutex<Option<UnboundedSender<Stamped>>> = Mutex::new(None);
    &BUS
}

fn send(cue: Cue) {
    let bus = bus().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(tx) = bus.as_ref() {
        let _ = tx.send((Instant::now(), cue));
    }
}

/// Record a `tui` mark into the open session, if any.
pub fn mark(m: Mark) {
    send(Cue::Mark(m));
}

/// What a session does with each cue besides writing it down. The live
/// stream presenter implements this; a dashboard run passes none, since
/// the dashboard already holds the event channel.
pub trait Viewer: Send {
    fn cue(&mut self, cue: &Cue, at: Instant);
    fn tick(&mut self, at: Instant);
    fn finish(&mut self, at: Instant);
}

/// A recording in progress. `open` claims the bus, `attach` taps a pass's
/// event channel, `close` drains and writes the last line.
pub struct Session {
    tx: UnboundedSender<Stamped>,
    task: JoinHandle<()>,
}

/// How often a viewer gets a tick (spinner frame, elapsed seconds).
pub const TICK: Duration = Duration::from_millis(250);

impl Session {
    /// Start a session writing to `path` (no file when `None`, which still
    /// feeds the viewer). The previous holder of the bus, if any, is
    /// replaced: one process records one run.
    pub fn open(path: Option<PathBuf>, viewer: Option<Box<dyn Viewer>>) -> Self {
        Stats::mark_start();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *bus().lock().unwrap_or_else(|p| p.into_inner()) = Some(tx.clone());
        let task = tokio::spawn(run_session(rx, path, viewer));
        Self { tx, task }
    }

    /// Tap `hooks.events` for one pipeline pass. A dashboard already holds
    /// the channel, so the tap tees; a headless run hands the session the
    /// channel outright, and the session's viewer draws the stream.
    pub fn attach(&self, mut hooks: crate::smith::EngineHooks) -> crate::smith::EngineHooks {
        let bus = self.tx.clone();
        let (up_tx, mut up_rx) = crate::engine::events::channel();
        let downstream = hooks.events.take();
        tokio::spawn(async move {
            while let Some(event) = up_rx.recv().await {
                if let Some(down) = &downstream {
                    let _ = down.send(event.clone());
                }
                let _ = bus.send((Instant::now(), Cue::Event(event)));
            }
        });
        hooks.events = Some(up_tx);
        hooks
    }

    /// Stop recording: release the bus, let the queue drain, and wait for
    /// the file and the viewer to finish. Bounded, so a stuck write cannot
    /// hold the exit hostage.
    pub async fn close(self) {
        let _ = self.tx.send((Instant::now(), Cue::Stats(Stats::capture())));
        {
            let mut bus = bus().lock().unwrap_or_else(|p| p.into_inner());
            *bus = None;
        }
        drop(self.tx);
        let _ = tokio::time::timeout(Duration::from_secs(3), self.task).await;
    }
}

async fn run_session(
    mut rx: UnboundedReceiver<Stamped>,
    path: Option<PathBuf>,
    mut viewer: Option<Box<dyn Viewer>>,
) {
    let base = Instant::now();
    let mut file = match &path {
        Some(p) => open_tape(p).await,
        None => None,
    };
    let mut snap = Snapshots::default();
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            cue = rx.recv() => {
                let Some((at, cue)) = cue else { break };
                let t = at.saturating_duration_since(base).as_millis() as u64;
                let (before, after) = snap.around(&cue);
                for c in before.into_iter().chain(std::iter::once(cue)).chain(after) {
                    if let Some(f) = file.as_mut() {
                        if let Some(line) = encode(t, &c) {
                            let ok = f.write_all(line.as_bytes()).await.is_ok()
                                && f.write_all(b"\n").await.is_ok()
                                && f.flush().await.is_ok();
                            if !ok {
                                file = None;
                            }
                        }
                    }
                    if let Some(v) = viewer.as_mut() {
                        v.cue(&c, at);
                    }
                }
            }
            _ = ticker.tick(), if viewer.is_some() => {
                if let Some(v) = viewer.as_mut() {
                    v.tick(Instant::now());
                }
            }
        }
    }
    if let Some(v) = viewer.as_mut() {
        v.finish(Instant::now());
    }
}

async fn open_tape(path: &Path) -> Option<tokio::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.ok()?;
        }
    }
    let mut f = tokio::fs::File::create(path).await.ok()?;
    let (cols, rows) = crossterm::terminal::size().ok().unzip();
    let meta = Meta {
        tape: TAPE_VERSION,
        version: env!("CARGO_PKG_VERSION").to_string(),
        started: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        cols,
        rows,
        argv: std::env::args().skip(1).collect(),
    };
    let line = serde_json::to_string(&meta).ok()?;
    f.write_all(line.as_bytes()).await.ok()?;
    f.write_all(b"\n").await.ok()?;
    Some(f)
}

/// Copies of the project files a replay needs, taken when they can have
/// changed and written only when they did, plus each heat's outcome.
#[derive(Default)]
struct Snapshots {
    plan: Option<String>,
    blueprint: Option<String>,
    ore: Option<String>,
    /// Heats still on the anvil: ingot, heat, and when it began.
    heats: Vec<(String, u8, std::time::SystemTime)>,
}

impl Snapshots {
    /// Cues to write before `cue` (a heat's outcome must land before the
    /// event that ends the heat) and after it (files that may have moved).
    fn around(&mut self, cue: &Cue) -> (Vec<Cue>, Vec<Cue>) {
        let mut before = Vec::new();
        // The forge marks a batch molten, saves, then starts each ingot: a
        // copy taken first shows the batch the start belongs to.
        if let Cue::Event(EngineEvent::IngotStart { .. }) = cue {
            before.extend(self.plan_if_moved());
        }
        match cue {
            Cue::Event(EngineEvent::HeatTick { id, heat }) => {
                if let Some(p) = self.close_heat(id, false) {
                    before.push(Cue::Proof(p));
                }
                self.heats.push((id.clone(), *heat, std::time::SystemTime::now()));
            }
            Cue::Event(EngineEvent::IngotDone { id, ok }) => {
                if let Some(p) = self.close_heat(id, *ok) {
                    before.push(Cue::Proof(p));
                }
            }
            // A rewind follows a failed heat at once; the outcome is known.
            Cue::Event(EngineEvent::Warning { message }) => {
                if let Some(id) = rewound(message) {
                    if let Some(p) = self.close_heat(id, false) {
                        before.push(Cue::Proof(p));
                    }
                }
            }
            _ => {}
        }
        (before, self.after(cue))
    }

    fn plan_if_moved(&mut self) -> Option<Cue> {
        let plan = std::fs::read_to_string(crate::config::CRUCIBLE).ok()?;
        if self.plan.as_deref() == Some(plan.as_str()) {
            return None;
        }
        self.plan = Some(plan.clone());
        Some(Cue::Plan(plan))
    }

    fn close_heat(&mut self, id: &str, ok: bool) -> Option<Proof> {
        let at = self.heats.iter().position(|(i, _, _)| i == id)?;
        let (id, heat, since) = self.heats.remove(at);
        Some(read_proof(std::path::Path::new(crate::config::LOG_DIR), &id, heat, ok, since))
    }

    fn after(&mut self, cue: &Cue) -> Vec<Cue> {
        let mut out = Vec::new();
        let plan_may_move = match cue {
            Cue::Mark(_) => true,
            Cue::Event(e) => matches!(
                e,
                EngineEvent::IngotStart { .. }
                    | EngineEvent::IngotDone { .. }
                    | EngineEvent::HeatTick { .. }
            ),
            _ => false,
        };
        if plan_may_move {
            out.extend(self.plan_if_moved());
        }
        if matches!(cue, Cue::Mark(_)) {
            if let Ok(doc) = std::fs::read_to_string(crate::config::ORE_FILE) {
                if self.ore.as_deref() != Some(doc.as_str()) {
                    self.ore = Some(doc.clone());
                    out.push(Cue::Doc { name: crate::config::ORE_FILE.to_string(), text: doc });
                }
            }
        }
        if matches!(cue, Cue::Mark(Mark::Status { .. })) {
            if let Ok(doc) = std::fs::read_to_string(crate::config::BLUEPRINT) {
                if self.blueprint.as_deref() != Some(doc.as_str()) {
                    self.blueprint = Some(doc.clone());
                    out.push(Cue::Doc { name: crate::config::BLUEPRINT.to_string(), text: doc });
                }
            }
        }
        out
    }
}

/// `[i2] rewound 3 file(s) to attempt start …` → `i2`.
pub fn rewound(message: &str) -> Option<&str> {
    let (id, rest) = message.strip_prefix('[')?.split_once("] ")?;
    rest.starts_with("rewound ").then_some(id)
}

/// Read a heat's outcome from the forge's logs: the smith's reply
/// (`*_STRIKE_<id>_<n>.log`, whose last `CMD:` line is the proof it
/// offered) and the command's run (`*_ASSAY_<id>_<n>.log`, `exit=` first).
/// Only files written since the heat began count, so an older run's logs
/// for the same id never speak for this one. Whatever is missing stays
/// `None`; the outcome itself always comes from the event.
pub fn read_proof(
    logs: &Path,
    id: &str,
    heat: u8,
    ok: bool,
    since: std::time::SystemTime,
) -> Proof {
    let strike = newest_log(logs, &format!("_STRIKE_{id}_"), since);
    let assay = newest_log(logs, &format!("_ASSAY_{id}_"), since);
    let cmd = strike.as_deref().and_then(crate::proof::extract_cmd);
    let why = if ok {
        None
    } else if strike.is_none() {
        Some("the smith stopped before offering a proof".to_string())
    } else if cmd.is_none() {
        Some("no CMD line in the reply".to_string())
    } else {
        match assay.as_deref().map(|a| a.split_once('\n').unwrap_or((a, ""))) {
            Some((exit, out)) if exit.trim() != "exit=0" => {
                Some(failure_gist(out).unwrap_or_else(|| "the proof exited non-zero".to_string()))
            }
            Some(_) => newest_log(logs, &format!("_PROOF_{id}_{heat}.log"), since)
                .and_then(|proof| proof.strip_prefix("exit=1\n")
                    .and_then(|out| out.lines().find(|l| !l.trim().is_empty()))
                    .map(|line| crate::engine::events::preview(line, 120)))
                .or_else(|| Some("the smith's check passed; the ingot's proof or bar did not".to_string())),
            None => Some("the proof never ran".to_string()),
        }
    };
    Proof { id: id.to_string(), heat, cmd, ok, why }
}

/// The line of a failing command's output that says why: an exception
/// with its message first (`AssertionError: 3 != 4`), then a FAIL line,
/// then anything mentioning an error, then the first line with words in it.
/// Rules and separators (`-----`) are never the reason.
pub fn failure_gist(out: &str) -> Option<String> {
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().any(char::is_alphanumeric))
        .collect();
    let rank = |l: &str| -> u8 {
        let low = l.to_lowercase();
        if l.contains("Error:") || l.contains("Exception:") {
            0
        } else if l.starts_with("FAIL") || low.contains("failed") {
            1
        } else if ["error", "not found", "no such", "unexpected", "cannot", "expected"]
            .iter()
            .any(|k| low.contains(k))
        {
            2
        } else {
            3
        }
    };
    let best = lines.iter().min_by_key(|l| rank(l))?;
    Some(crate::engine::events::preview(best, 120))
}

fn newest_log(dir: &Path, needle: &str, since: std::time::SystemTime) -> Option<String> {
    let mut best: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.contains(needle) || !name.ends_with(".log") {
            continue;
        }
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|m| m + Duration::from_secs(1) >= since);
        if fresh && best.as_ref().is_none_or(|(b, _)| name > *b) {
            best = Some((name, entry.path()));
        }
    }
    std::fs::read_to_string(best?.1).ok()
}

/// `logs/tape-<ts>.jsonl`, beside the run's other logs.
pub fn default_path() -> PathBuf {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    PathBuf::from(crate::config::LOG_DIR).join(format!("tape-{ts}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tape_of(lines: &[&str]) -> Tape {
        parse(&lines.join("\n"))
    }

    #[test]
    fn events_round_trip_through_a_tape_line() {
        let events = [
            EngineEvent::TurnStart { turn: 2 },
            EngineEvent::ToolCallStart { name: "bash".into(), preview: "{\"command\":\"ls\"}".into() },
            EngineEvent::ToolResult {
                name: "bash".into(),
                ok: false,
                preview: "boom".into(),
                lines: 3,
                bytes: 40,
                ms: 12,
            },
            EngineEvent::Tokens {
                usage: Usage { total_tokens: 900, cost: Some(0.01), ..Default::default() },
            },
            EngineEvent::IngotDone { id: "i2".into(), ok: true },
            EngineEvent::DuelVerdict { id: "i1".into(), winner: 'b', margin: 3 },
            EngineEvent::HookFinished {
                name: "fmt".into(),
                hook_event: "post_tool".into(),
                code: 2,
                duration_ms: 40,
            },
        ];
        for (i, ev) in events.iter().enumerate() {
            let line = encode(i as u64 * 10, &Cue::Event(ev.clone())).unwrap();
            assert!(line.starts_with(&format!("{{\"t\":{}", i * 10)), "{line}");
            let tape = parse(&line);
            let Cue::Event(back) = &tape.cues[0].cue else { panic!("{line}") };
            assert_eq!(format!("{back:?}"), format!("{ev:?}"));
            assert_eq!(tape.cues[0].t, i as u64 * 10);
        }
    }

    #[test]
    fn marks_plans_and_docs_round_trip() {
        let cues = [
            Cue::Mark(Mark::Banner),
            Cue::Mark(Mark::Header { text: "FORGE".into() }),
            Cue::Mark(Mark::Status { icon: "█".into(), tone: Tone::Pure, text: "Mold".into() }),
            Cue::Mark(Mark::Counts { tally: Tally { total: 3, forged: 1, ..Default::default() } }),
            Cue::Plan("(ingot :id \"i1\")".into()),
            Cue::Doc { name: "BLUEPRINT.md".into(), text: "# B".into() },
        ];
        for cue in cues {
            let line = encode(5, &cue).unwrap();
            let back = parse(&line);
            assert_eq!(back.skipped, 0, "{line}");
            assert_eq!(format!("{:?}", back.cues[0].cue), format!("{cue:?}"));
        }
    }

    #[test]
    fn an_untimed_engine_log_gets_a_monotonic_synthetic_clock() {
        let tape = tape_of(&[
            r#"{"event":"turn_start","turn":1}"#,
            r#"{"event":"model_call","model":"m"}"#,
            r#"{"event":"tool_call_start","name":"bash","preview":"ls"}"#,
            r#"{"event":"tool_result","name":"bash","ok":true,"preview":"ok"}"#,
        ]);
        let ts: Vec<u64> = tape.cues.iter().map(|c| c.t).collect();
        assert_eq!(ts[0], 0);
        assert!(ts.windows(2).all(|w| w[0] < w[1]), "{ts:?}");
    }

    #[test]
    fn a_torn_line_is_skipped_not_fatal() {
        let tape = tape_of(&[
            r#"{"tape":1,"version":"2.9.0"}"#,
            r#"{"t":1,"event":"turn_start","turn":1}"#,
            r#"{"t":2,"event":"tool_ca"#,
            r#"{"t":3,"event":"no_such_event"}"#,
        ]);
        assert_eq!(tape.cues.len(), 1);
        assert_eq!(tape.skipped, 2);
        assert_eq!(tape.meta.unwrap().version, "2.9.0");
    }

    #[test]
    fn a_failure_gist_skips_rules_and_prefers_the_exception() {
        let unittest = "F.\n======\nFAIL: test_top (tests.test_core.T)\n------\nTraceback (most recent call last):\n  File \"x\", line 3\nAssertionError: 3 != 4\n------\nRan 2 tests\nFAILED (failures=1)\n";
        assert_eq!(failure_gist(unittest).as_deref(), Some("AssertionError: 3 != 4"));
        assert_eq!(failure_gist("-----\n=====\n").as_deref(), None);
        assert_eq!(failure_gist("grep: x: No such file or directory").as_deref(), Some("grep: x: No such file or directory"));
    }

    #[test]
    fn a_heat_outcome_is_read_from_its_own_logs() {
        let dir = tempfile::tempdir().unwrap();
        let since = std::time::SystemTime::now() - Duration::from_secs(5);
        std::fs::write(dir.path().join("20261015_100000_STRIKE_i2_1.log"), "did it\nCMD: node --test\n").unwrap();
        std::fs::write(dir.path().join("20261015_100001_ASSAY_i2_1.log"), "exit=1\nnot ok 1 - adds\nError: expected 3\n").unwrap();
        let p = read_proof(dir.path(), "i2", 1, false, since);
        assert_eq!(p.cmd.as_deref(), Some("node --test"));
        assert_eq!(p.why.as_deref(), Some("Error: expected 3"));
        // Another ingot's logs never speak for this one.
        let q = read_proof(dir.path(), "i3", 1, false, since);
        assert_eq!(q.cmd, None);
        assert!(q.why.unwrap().contains("smith stopped"));
        // A pass carries its command and no reason.
        let ok = read_proof(dir.path(), "i2", 1, true, since);
        assert_eq!((ok.cmd.as_deref(), ok.why), (Some("node --test"), None));
    }

    #[test]
    fn a_failed_ingot_proof_names_the_shell_error_after_a_passing_cmd() {
        let dir = tempfile::tempdir().unwrap();
        let since = std::time::SystemTime::now() - Duration::from_secs(5);
        for (name, body) in [
            ("STRIKE_i1_1", "CMD: true\n"),
            ("ASSAY_i1_1", "exit=0\n"),
            ("PROOF_i1_1", "exit=1\nbash: -c: line 1: syntax error near unexpected token `('\nsecond line\n"),
        ] {
            std::fs::write(dir.path().join(format!("20261017_100000_{name}.log")), body).unwrap();
        }
        let proof = read_proof(dir.path(), "i1", 1, false, since);
        assert_eq!(proof.why.as_deref(), Some("bash: -c: line 1: syntax error near unexpected token `('"));
        let other_heat = read_proof(dir.path(), "i1", 2, false, since);
        assert!(!other_heat.why.unwrap().contains("syntax error"));
        std::fs::write(dir.path().join("20261017_100001_PROOF_i1_1.log"), "exit=0\n").unwrap();
        let rewritten = read_proof(dir.path(), "i1", 1, false, since);
        assert!(!rewritten.why.unwrap().contains("syntax error"));
    }

    #[test]
    fn locate_reads_offsets_and_named_points() {
        let tape = tape_of(&[
            r#"{"t":0,"mark":"banner"}"#,
            r#"{"t":10,"mark":"header","text":"FORGE"}"#,
            r#"{"t":100,"event":"ingot_start","id":"i1","work":"a"}"#,
            r#"{"t":110,"event":"ingot_start","id":"i2","work":"b"}"#,
            r#"{"t":120,"event":"heat_tick","id":"i1","heat":1}"#,
            r#"{"t":200,"event":"heat_tick","id":"i1","heat":2}"#,
            r#"{"t":300,"event":"ingot_done","id":"i2","ok":false}"#,
            r#"{"t":400,"event":"warning","message":"warden round 1: no tests"}"#,
            r#"{"t":1000,"event":"ingot_done","id":"i1","ok":true}"#,
        ]);
        assert_eq!(locate(&tape, "50%").unwrap(), 500);
        assert_eq!(locate(&tape, "0.2s").unwrap(), 200);
        assert_eq!(locate(&tape, "250ms").unwrap(), 250);
        assert_eq!(locate(&tape, "assay").unwrap(), 1000);
        assert!(locate(&tape, "retry").unwrap() >= 200);
        assert!(locate(&tape, "crack").unwrap() >= 300);
        assert!(locate(&tape, "warden").unwrap() >= 400);
        assert!(locate(&tape, "anvils").unwrap() >= 110);
        let err = locate(&tape, "nope").unwrap_err();
        assert!(err.contains("retry"), "{err}");
        assert!(locate(&tape, "plan").is_err(), "no mold in this tape");
    }
}
