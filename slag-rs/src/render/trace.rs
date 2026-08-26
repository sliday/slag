//! trace — Chrome trace-event export (`--trace trace.json`).
//!
//! A second event sink beside the JSONL one. Where JSONL answers "what
//! happened", the trace answers "when, for how long, and in parallel with
//! what": load the file in `chrome://tracing` or Perfetto and each ingot is
//! a bar on its own lane, each tool call a nested bar inside it.
//!
//! The mapper is pure and IO-free, like `render/diff.rs`. It turns
//! `EngineEvent`s into duration events (`ph: "B"` / `ph: "E"`) and lets the
//! sink worry about files.
//!
//! Lane assignment is the interesting half. `hooks.events` merges every
//! anvil onto one channel, and only ingot events carry an id, so:
//!
//! - each open ingot holds a lane, freed on `IngotDone` and reused, so three
//!   parallel forges read as three lanes and a serialized run as a staircase
//!   down one;
//! - tool events land on the sole open ingot's lane when exactly one is open
//!   (the serial case, where attribution is certain) and on the shared lane 0
//!   otherwise, rather than guessing an owner.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use crate::engine::{EngineEvent, Usage};

/// Lane shared by tool calls that cannot be attributed to a single anvil.
const SHARED_LANE: u64 = 0;
/// First lane handed to an ingot. Lane 0 stays reserved for shared work.
const FIRST_INGOT_LANE: u64 = 1;

/// One Chrome trace event. `ts`/`dur` are microseconds; `pid` is fixed at 1
/// since a run is one process.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceEvent {
    pub name: String,
    pub ph: &'static str,
    pub ts: u64,
    pub pid: u64,
    pub tid: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

impl TraceEvent {
    fn begin(name: impl Into<String>, ts: u64, tid: u64) -> Self {
        Self { name: name.into(), ph: "B", ts, pid: 1, tid, args: None }
    }

    fn end(name: impl Into<String>, ts: u64, tid: u64) -> Self {
        Self { name: name.into(), ph: "E", ts, pid: 1, tid, args: None }
    }

    fn with_args(mut self, args: serde_json::Value) -> Self {
        self.args = Some(args);
        self
    }
}

/// Turns a stream of `EngineEvent`s into Chrome trace events.
///
/// Deliberately clock-free: the caller supplies each event's timestamp in
/// microseconds, so tests are deterministic and the sink can use one
/// monotonic base for the whole run.
#[derive(Debug, Default)]
pub struct TraceMapper {
    /// Lane held by each open ingot, keyed by ingot id.
    lanes: HashMap<String, u64>,
    /// Unmatched tool `B` events per lane, innermost last.
    tool_stack: HashMap<u64, Vec<String>>,
    /// Usage accumulated since the last `IngotDone`, so a finished ingot can
    /// report what it burned.
    pending: Usage,
}

impl TraceMapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Map one event. Most events produce nothing; the ones that do produce
    /// exactly one trace event.
    pub fn map(&mut self, event: &EngineEvent, ts: u64) -> Option<TraceEvent> {
        match event {
            EngineEvent::IngotStart { id, work } => {
                let tid = self.claim_lane(id);
                Some(
                    TraceEvent::begin(format!("ingot {id}"), ts, tid)
                        .with_args(serde_json::json!({ "id": id, "work": work })),
                )
            }
            EngineEvent::IngotDone { id, ok } => {
                let tid = self.lanes.remove(id)?;
                let usage = std::mem::take(&mut self.pending);
                Some(TraceEvent::end(format!("ingot {id}"), ts, tid).with_args(
                    serde_json::json!({
                        "id": id,
                        "ok": ok,
                        "tokens": usage.total_tokens,
                        "prompt_tokens": usage.prompt_tokens,
                        "completion_tokens": usage.completion_tokens,
                        "cost": usage.cost,
                    }),
                ))
            }
            EngineEvent::ToolCallStart { name, preview } => {
                let tid = self.tool_lane();
                self.tool_stack.entry(tid).or_default().push(name.clone());
                Some(
                    TraceEvent::begin(format!("tool {name}"), ts, tid)
                        .with_args(serde_json::json!({ "preview": preview })),
                )
            }
            EngineEvent::ToolResult { name, ok, lines, bytes, ms, .. } => {
                let tid = self.pop_tool_lane(name)?;
                Some(TraceEvent::end(format!("tool {name}"), ts, tid).with_args(
                    serde_json::json!({
                        "ok": ok,
                        "lines": lines,
                        "bytes": bytes,
                        "ms": ms,
                    }),
                ))
            }
            EngineEvent::Tokens { usage } => {
                self.pending.add(usage);
                None
            }
            _ => None,
        }
    }

    /// Lowest free lane at or above `FIRST_INGOT_LANE`, so a serial run keeps
    /// reusing lane 1 instead of drifting rightward forever.
    fn claim_lane(&mut self, id: &str) -> u64 {
        if let Some(tid) = self.lanes.get(id) {
            return *tid;
        }
        let mut tid = FIRST_INGOT_LANE;
        while self.lanes.values().any(|held| *held == tid) {
            tid += 1;
        }
        self.lanes.insert(id.to_string(), tid);
        tid
    }

    /// A tool call belongs to the open ingot only when there is exactly one;
    /// with two anvils running, the merged channel cannot say which.
    fn tool_lane(&self) -> u64 {
        if self.lanes.len() == 1 {
            self.lanes.values().copied().next().unwrap_or(SHARED_LANE)
        } else {
            SHARED_LANE
        }
    }

    /// Close the innermost open `B` for `name`. Matching by name rather than
    /// by lane keeps a result paired with its own call when two anvils
    /// interleave on the shared lane.
    fn pop_tool_lane(&mut self, name: &str) -> Option<u64> {
        let mut fallback = None;
        for (tid, stack) in self.tool_stack.iter_mut() {
            if let Some(pos) = stack.iter().rposition(|open| open == name) {
                stack.remove(pos);
                return Some(*tid);
            }
            if !stack.is_empty() {
                fallback = Some(*tid);
            }
        }
        // A result with no matching call (truncated log, renamed tool) still
        // has to close *something*, or every later bar nests inside it.
        let tid = fallback?;
        if let Some(stack) = self.tool_stack.get_mut(&tid) {
            stack.pop();
        }
        Some(tid)
    }

    /// `E` events closing everything still open, for a run cut short by
    /// Ctrl-C. Without them the viewer stretches the last bar to infinity.
    pub fn close_open(&mut self, ts: u64) -> Vec<TraceEvent> {
        let mut out = Vec::new();
        for (tid, stack) in self.tool_stack.iter_mut() {
            while let Some(name) = stack.pop() {
                out.push(TraceEvent::end(format!("tool {name}"), ts, *tid));
            }
        }
        let mut open: Vec<_> = self.lanes.drain().collect();
        open.sort_by_key(|(_, tid)| *tid);
        for (id, tid) in open {
            out.push(TraceEvent::end(format!("ingot {id}"), ts, tid));
        }
        out
    }
}

/// Spawn a task writing a Chrome trace JSON array to `path`.
///
/// The file is a JSON *array*, not JSONL, so the closing bracket matters:
/// a run that never writes it produces a file no viewer will load. The task
/// writes it on drain, which covers Ctrl-C too — the shutdown registry drops
/// every `EventTx`, the channel closes, and this loop falls through to the
/// close. Anything still open gets a synthetic `E` first.
/// Production always goes through `attach`, which owns the shutdown-close
/// flag; this plain form exists for tests that exercise the drain path on
/// its own.
#[cfg(test)]
fn spawn_trace_sink(rx: UnboundedReceiver<EngineEvent>, path: PathBuf) -> JoinHandle<()> {
    spawn_trace_sink_closed_by(rx, path, Arc::new(AtomicBool::new(false)))
}

/// `spawn_trace_sink` with the "already closed" flag exposed, so a
/// synchronous cleanup can close the array when the async task will not get
/// the chance. Whoever flips the flag first writes the bracket; the loser
/// leaves the file alone, since a second `]` is as unloadable as none.
fn spawn_trace_sink_closed_by(
    mut rx: UnboundedReceiver<EngineEvent>,
    path: PathBuf,
    closed: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut file = match open_trace(&path).await {
            Some(f) => f,
            None => {
                closed.store(true, Ordering::SeqCst);
                // Drain so the fanout never blocks on a dead sink.
                while rx.recv().await.is_some() {}
                return;
            }
        };
        let base = std::time::Instant::now();
        let mut mapper = TraceMapper::new();
        let mut first = true;

        while let Some(event) = rx.recv().await {
            let ts = base.elapsed().as_micros() as u64;
            if let Some(te) = mapper.map(&event, ts) {
                if write_event(&mut file, &te, &mut first).await.is_err() {
                    while rx.recv().await.is_some() {}
                    return;
                }
            }
        }

        // Claim the close before writing anything else. A cleanup that
        // already closed the array left the bracket at the end of the file,
        // while this handle's offset still points before it — writing on
        // would overwrite the bracket and undo the rescue.
        if closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let ts = base.elapsed().as_micros() as u64;
        for te in mapper.close_open(ts) {
            if write_event(&mut file, &te, &mut first).await.is_err() {
                return;
            }
        }
        let _ = file.write_all(b"\n]\n").await;
        let _ = file.flush().await;
    })
}

/// Append the closing `]` to a trace file, synchronously, exactly once.
///
/// The shell-level Ctrl-C path ends in `std::process::exit(130)`, which
/// never lets a tokio task finish — so the async sink's own close is not
/// reachable there. `shutdown::register` runs this instead. Only the array
/// bracket is written: without it no viewer loads the file at all, whereas
/// a bar left open just renders as running to the end of the trace.
fn close_trace_file(path: &PathBuf, closed: &Arc<AtomicBool>) {
    use std::io::Write;

    if closed.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = f.write_all(b"\n]\n");
        let _ = f.flush();
    }
}

/// Wire a trace sink into `hooks`, returning the hooks the pipeline should
/// run with and the sink's handle.
///
/// `hooks.events` is the one channel carrying both pipeline events
/// (`IngotStart`/`IngotDone` from forge) and agent events (fanned in from
/// each smith), which is why the tap goes here rather than at any single
/// smith. When the dashboard already holds that channel, a tee duplicates
/// into both; headless, the trace sink takes it outright — which is what
/// makes `--trace` work without `--tui`.
pub fn attach(
    mut hooks: crate::smith::EngineHooks,
    path: Option<PathBuf>,
) -> (crate::smith::EngineHooks, Option<JoinHandle<()>>) {
    let Some(path) = path else {
        return (hooks, None);
    };
    let (trace_tx, trace_rx) = crate::engine::events::channel();
    // A shell Ctrl-C exits the process outright, so the sink's own close is
    // unreachable there; the shutdown registry carries it instead. Whichever
    // path runs first wins the flag, so the bracket is written exactly once.
    let closed = Arc::new(AtomicBool::new(false));
    let sink = spawn_trace_sink_closed_by(trace_rx, path.clone(), closed.clone());
    crate::shutdown::register(move || close_trace_file(&path, &closed));

    match hooks.events.take() {
        Some(downstream) => {
            let (up_tx, mut up_rx) = crate::engine::events::channel();
            tokio::spawn(async move {
                while let Some(event) = up_rx.recv().await {
                    let _ = downstream.send(event.clone());
                    let _ = trace_tx.send(event);
                }
            });
            hooks.events = Some(up_tx);
        }
        None => hooks.events = Some(trace_tx),
    }
    (hooks, Some(sink))
}

async fn open_trace(path: &PathBuf) -> Option<tokio::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.ok()?;
        }
    }
    let mut file = tokio::fs::File::create(path).await.ok()?;
    file.write_all(b"[\n").await.ok()?;
    Some(file)
}

async fn write_event(
    file: &mut tokio::fs::File,
    event: &TraceEvent,
    first: &mut bool,
) -> std::io::Result<()> {
    let line = serde_json::to_string(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if !*first {
        file.write_all(b",\n").await?;
    }
    *first = false;
    file.write_all(line.as_bytes()).await?;
    file.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ingot_start(id: &str) -> EngineEvent {
        EngineEvent::IngotStart { id: id.into(), work: "w".into() }
    }

    fn ingot_done(id: &str, ok: bool) -> EngineEvent {
        EngineEvent::IngotDone { id: id.into(), ok }
    }

    fn tool_start(name: &str) -> EngineEvent {
        EngineEvent::ToolCallStart { name: name.into(), preview: "p".into() }
    }

    fn tool_result(name: &str, ms: u64) -> EngineEvent {
        EngineEvent::ToolResult {
            name: name.into(),
            ok: true,
            preview: "p".into(),
            lines: 3,
            bytes: 90,
            ms,
        }
    }

    #[test]
    fn ingot_maps_to_a_matched_begin_end_pair_on_one_lane() {
        let mut m = TraceMapper::new();
        let b = m.map(&ingot_start("i1"), 100).expect("B");
        let e = m.map(&ingot_done("i1", true), 500).expect("E");

        assert_eq!(b.ph, "B");
        assert_eq!(e.ph, "E");
        assert_eq!(b.name, "ingot i1");
        assert_eq!(e.name, b.name, "B and E must share a name to pair");
        assert_eq!(e.tid, b.tid, "B and E must share a lane to pair");
        assert_eq!((b.ts, e.ts), (100, 500));
        assert_eq!(b.pid, 1);
    }

    #[test]
    fn two_concurrent_ingots_take_separate_lanes() {
        let mut m = TraceMapper::new();
        let a = m.map(&ingot_start("i1"), 0).expect("B i1");
        let b = m.map(&ingot_start("i2"), 10).expect("B i2");

        assert_ne!(a.tid, b.tid, "parallel ingots must not share a lane");
        assert_ne!(a.tid, SHARED_LANE);
        assert_ne!(b.tid, SHARED_LANE);

        let a_end = m.map(&ingot_done("i1", true), 20).expect("E i1");
        let b_end = m.map(&ingot_done("i2", false), 30).expect("E i2");
        assert_eq!(a_end.tid, a.tid);
        assert_eq!(b_end.tid, b.tid);
    }

    #[test]
    fn a_serial_run_reuses_the_first_lane_instead_of_drifting() {
        let mut m = TraceMapper::new();
        let first = m.map(&ingot_start("i1"), 0).expect("B");
        m.map(&ingot_done("i1", true), 1);
        let second = m.map(&ingot_start("i2"), 2).expect("B");

        assert_eq!(first.tid, second.tid, "a freed lane is reused");
        assert_eq!(first.tid, FIRST_INGOT_LANE);
    }

    #[test]
    fn tool_calls_nest_on_the_lane_of_the_only_open_ingot() {
        let mut m = TraceMapper::new();
        let ingot = m.map(&ingot_start("i1"), 0).expect("B ingot");
        let tb = m.map(&tool_start("read"), 5).expect("B tool");
        let te = m.map(&tool_result("read", 42), 9).expect("E tool");

        assert_eq!(tb.tid, ingot.tid, "serial tools nest inside their ingot");
        assert_eq!(te.tid, ingot.tid);
        assert_eq!(tb.name, "tool read");
        assert_eq!(te.name, tb.name);
    }

    #[test]
    fn tools_fall_back_to_the_shared_lane_when_two_anvils_are_open() {
        let mut m = TraceMapper::new();
        m.map(&ingot_start("i1"), 0);
        m.map(&ingot_start("i2"), 1);
        let tb = m.map(&tool_start("bash"), 2).expect("B tool");

        assert_eq!(tb.tid, SHARED_LANE, "ambiguous attribution goes to lane 0");
    }

    #[test]
    fn tool_result_carries_the_duration_measured_by_item_63() {
        let mut m = TraceMapper::new();
        m.map(&ingot_start("i1"), 0);
        m.map(&tool_start("bash"), 1);
        let te = m.map(&tool_result("bash", 1234), 2).expect("E tool");

        let args = te.args.expect("args");
        assert_eq!(args["ms"], 1234);
        assert_eq!(args["lines"], 3);
        assert_eq!(args["bytes"], 90);
        assert_eq!(args["ok"], true);
    }

    #[test]
    fn ingot_end_args_carry_the_tokens_and_cost_burned() {
        let mut m = TraceMapper::new();
        m.map(&ingot_start("i1"), 0);
        m.map(
            &EngineEvent::Tokens {
                usage: Usage {
                    prompt_tokens: 100,
                    completion_tokens: 20,
                    total_tokens: 120,
                    cost: Some(0.5),
                    ..Usage::default()
                },
            },
            1,
        );
        m.map(
            &EngineEvent::Tokens {
                usage: Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    cost: Some(0.25),
                    ..Usage::default()
                },
            },
            2,
        );
        let end = m.map(&ingot_done("i1", true), 3).expect("E");

        let args = end.args.expect("args");
        assert_eq!(args["tokens"], 135);
        assert_eq!(args["prompt_tokens"], 110);
        assert_eq!(args["completion_tokens"], 25);
        assert_eq!(args["cost"], 0.75);
        assert_eq!(args["ok"], true);
    }

    #[test]
    fn usage_resets_between_ingots() {
        let mut m = TraceMapper::new();
        m.map(&ingot_start("i1"), 0);
        m.map(
            &EngineEvent::Tokens {
                usage: Usage { total_tokens: 99, ..Default::default() },
            },
            1,
        );
        m.map(&ingot_done("i1", true), 2);

        m.map(&ingot_start("i2"), 3);
        let end = m.map(&ingot_done("i2", true), 4).expect("E");
        assert_eq!(end.args.expect("args")["tokens"], 0, "i2 did not burn i1's tokens");
    }

    #[test]
    fn events_outside_the_mapping_produce_nothing() {
        let mut m = TraceMapper::new();
        assert!(m.map(&EngineEvent::TurnStart { turn: 1 }, 0).is_none());
        assert!(m.map(&EngineEvent::ModelCall { model: "x".into() }, 0).is_none());
        assert!(m.map(&EngineEvent::Finish { summary: "s".into() }, 0).is_none());
    }

    #[test]
    fn an_unmatched_ingot_done_is_dropped_rather_than_closing_a_stranger() {
        let mut m = TraceMapper::new();
        assert!(m.map(&ingot_done("ghost", true), 0).is_none());
    }

    #[test]
    fn close_open_ends_every_bar_a_ctrl_c_left_hanging() {
        let mut m = TraceMapper::new();
        m.map(&ingot_start("i1"), 0);
        m.map(&ingot_start("i2"), 1);
        m.map(&tool_start("bash"), 2);

        let closers = m.close_open(500);
        assert_eq!(closers.len(), 3, "one tool and two ingots stay open");
        assert!(closers.iter().all(|e| e.ph == "E"));
        assert!(closers.iter().all(|e| e.ts == 500));
        assert!(closers.iter().any(|e| e.name == "tool bash"));
        assert!(closers.iter().any(|e| e.name == "ingot i1"));
        assert!(closers.iter().any(|e| e.name == "ingot i2"));

        assert!(m.close_open(600).is_empty(), "a second close finds nothing open");
    }

    #[test]
    fn interleaved_tools_on_the_shared_lane_pair_by_name() {
        let mut m = TraceMapper::new();
        m.map(&ingot_start("i1"), 0);
        m.map(&ingot_start("i2"), 1);
        m.map(&tool_start("read"), 2);
        m.map(&tool_start("bash"), 3);

        // The bash result arrives first; it must close bash, not read.
        let bash_end = m.map(&tool_result("bash", 1), 4).expect("E bash");
        assert_eq!(bash_end.name, "tool bash");
        let read_end = m.map(&tool_result("read", 1), 5).expect("E read");
        assert_eq!(read_end.name, "tool read");

        // Only the two ingots stay open; no tool bar is left dangling.
        let closers = m.close_open(6);
        assert!(
            closers.iter().all(|e| e.name.starts_with("ingot ")),
            "both tools were already closed, so nothing tool-shaped remains: {closers:?}"
        );
    }

    #[test]
    fn serialized_json_is_the_shape_chrome_tracing_reads() {
        let mut m = TraceMapper::new();
        let b = m.map(&ingot_start("i1"), 250).expect("B");
        let json = serde_json::to_value(&b).expect("serialize");

        assert_eq!(json["ph"], "B");
        assert_eq!(json["ts"], 250);
        assert_eq!(json["pid"], 1);
        assert_eq!(json["name"], "ingot i1");
        assert!(json["tid"].is_number());
    }

    #[tokio::test]
    async fn the_sink_writes_a_closed_json_array_a_viewer_can_load() {
        let dir = std::env::temp_dir().join(format!("slag-trace-{}", std::process::id()));
        let path = dir.join("trace.json");
        let _ = std::fs::remove_file(&path);

        let (tx, rx) = crate::engine::events::channel();
        let task = spawn_trace_sink(rx, path.clone());
        tx.send(ingot_start("i1")).unwrap();
        tx.send(tool_start("read")).unwrap();
        tx.send(tool_result("read", 7)).unwrap();
        tx.send(ingot_done("i1", true)).unwrap();
        drop(tx);
        task.await.unwrap();

        let body = std::fs::read_to_string(&path).expect("trace file");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("a closed, parseable array");
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0]["ph"], "B");
        assert_eq!(parsed[3]["ph"], "E");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_hard_exit_still_closes_the_array_from_the_shutdown_registry() {
        // The shell Ctrl-C path never lets the sink task finish, so this is
        // the close that actually runs there.
        let dir = std::env::temp_dir().join(format!("slag-trace-sigint-{}", std::process::id()));
        let path = dir.join("trace.json");
        let _ = std::fs::remove_file(&path);

        let closed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crate::engine::events::channel();
        let sink = spawn_trace_sink_closed_by(rx, path.clone(), closed.clone());
        tx.send(ingot_start("i1")).unwrap();

        // Let the sink write the B before the "signal" lands.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        close_trace_file(&path, &closed);

        let body = std::fs::read_to_string(&path).expect("trace file");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("loadable despite the hard exit");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["ph"], "B");

        // And the sink, draining later, must not append a second bracket.
        drop(tx);
        sink.await.unwrap();
        let body = std::fs::read_to_string(&path).expect("trace file");
        assert_eq!(body.matches(']').count(), 1, "closed exactly once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_an_already_closed_trace_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("slag-trace-twice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.json");
        std::fs::write(&path, "[\n").unwrap();

        let closed = Arc::new(AtomicBool::new(false));
        close_trace_file(&path, &closed);
        close_trace_file(&path, &closed);

        let body = std::fs::read_to_string(&path).expect("trace file");
        assert_eq!(body.matches(']').count(), 1, "a second ] would break the parse");
        assert!(serde_json::from_str::<Vec<serde_json::Value>>(&body).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn attach_without_a_path_leaves_the_hooks_untouched() {
        let (tx, _rx) = crate::engine::events::channel();
        let hooks = crate::smith::EngineHooks { events: Some(tx), ..Default::default() };
        let (out, sink) = attach(hooks, None);

        assert!(sink.is_none(), "no --trace, no sink");
        assert!(out.events.is_some(), "the dashboard channel survives");
    }

    #[tokio::test]
    async fn attach_headless_routes_events_straight_to_the_trace() {
        let dir = std::env::temp_dir().join(format!("slag-trace-headless-{}", std::process::id()));
        let path = dir.join("trace.json");
        let _ = std::fs::remove_file(&path);

        let (hooks, sink) =
            attach(crate::smith::EngineHooks::default(), Some(path.clone()));
        let tx = hooks.events.clone().expect("trace took the channel");
        tx.send(ingot_start("i1")).unwrap();
        tx.send(ingot_done("i1", true)).unwrap();
        drop(tx);
        drop(hooks);
        sink.expect("sink").await.unwrap();

        let body = std::fs::read_to_string(&path).expect("trace file");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).expect("array");
        assert_eq!(parsed.len(), 2, "--trace works without --tui");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn attach_tees_so_the_dashboard_keeps_seeing_every_event() {
        let dir = std::env::temp_dir().join(format!("slag-trace-tee-{}", std::process::id()));
        let path = dir.join("trace.json");
        let _ = std::fs::remove_file(&path);

        let (dash_tx, mut dash_rx) = crate::engine::events::channel();
        let hooks = crate::smith::EngineHooks { events: Some(dash_tx), ..Default::default() };
        let (hooks, sink) = attach(hooks, Some(path.clone()));

        let tx = hooks.events.clone().expect("teed channel");
        tx.send(ingot_start("i1")).unwrap();
        tx.send(ingot_done("i1", true)).unwrap();
        drop(tx);
        drop(hooks);
        sink.expect("sink").await.unwrap();

        let mut seen = 0;
        while dash_rx.recv().await.is_some() {
            seen += 1;
        }
        assert_eq!(seen, 2, "the dashboard still gets both events");

        let body = std::fs::read_to_string(&path).expect("trace file");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).expect("array");
        assert_eq!(parsed.len(), 2, "and so does the trace");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_run_cut_short_still_yields_a_loadable_trace() {
        let dir = std::env::temp_dir().join(format!("slag-trace-cut-{}", std::process::id()));
        let path = dir.join("trace.json");
        let _ = std::fs::remove_file(&path);

        let (tx, rx) = crate::engine::events::channel();
        let task = spawn_trace_sink(rx, path.clone());
        tx.send(ingot_start("i1")).unwrap();
        tx.send(tool_start("bash")).unwrap();
        // No results, no IngotDone: the Ctrl-C shape.
        drop(tx);
        task.await.unwrap();

        let body = std::fs::read_to_string(&path).expect("trace file");
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("a closed array despite the cut");
        let ends = parsed.iter().filter(|e| e["ph"] == "E").count();
        assert_eq!(ends, 2, "the open tool and the open ingot both got closed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
