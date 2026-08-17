//! events — engine event plumbing: channel, JSONL sink, stderr narrator.
//!
//! One typed event stream (`EngineEvent` in `engine::mod`) feeds three
//! consumers: the JSONL log sink in `logs/`, the compact stderr narrator
//! (stream-mode display until the Ratatui dashboard lands), and a future
//! `--json` print mode. Display must never kill the forge: every IO error
//! here is warned once and then swallowed.

use std::path::PathBuf;

use crossterm::style::{ResetColor, SetForegroundColor};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use super::{EngineEvent, EventTx};
use crate::tui::{BRIGHT, COLD, HOT, PURE, WARM};

/// Create the engine event channel. Sender clones cheaply into the agent
/// loop; the receiver goes to exactly one sink (`spawn_jsonl_sink` or
/// `StderrNarrator::spawn_narrator`).
pub fn channel() -> (EventTx, UnboundedReceiver<EngineEvent>) {
    tokio::sync::mpsc::unbounded_channel()
}

/// Single-line, char-boundary-safe truncation with ellipsis. Collapses
/// whitespace runs (including newlines) to single spaces so tool previews
/// stay one-liners in both the narrator and the JSONL stream.
pub fn preview(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }

    let mut line = String::with_capacity(s.len().min(max * 4));
    let mut last_space = true; // leading whitespace drops
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_space {
                line.push(' ');
                last_space = true;
            }
        } else if ch.is_control() {
            // Drop ESC and every other control char so tool output can
            // never inject ANSI sequences into the terminal or dashboard.
        } else {
            line.push(ch);
            last_space = false;
        }
    }
    let line = line.trim_end().to_string();

    if line.chars().count() <= max {
        return line;
    }
    let cut: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Spawn a task that appends one serde_json line per event to `path`,
/// flushing per line. Parent directories are created. IO errors are
/// warned to stderr once, then swallowed — the forge keeps running.
pub fn spawn_jsonl_sink(
    mut rx: UnboundedReceiver<EngineEvent>,
    path: PathBuf,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut warned = false;
        let mut file = open_append(&path, &mut warned).await;

        while let Some(event) = rx.recv().await {
            let Some(f) = file.as_mut() else {
                continue; // sink is dead; drain quietly
            };
            let line = match serde_json::to_string(&event) {
                Ok(line) => line,
                Err(e) => {
                    warn_once(&mut warned, &path, &e.to_string());
                    continue;
                }
            };
            let write = async {
                f.write_all(line.as_bytes()).await?;
                f.write_all(b"\n").await?;
                f.flush().await
            };
            if let Err(e) = write.await {
                warn_once(&mut warned, &path, &e.to_string());
                file = None;
            }
        }
    })
}

async fn open_append(path: &PathBuf, warned: &mut bool) -> Option<tokio::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                warn_once(warned, path, &e.to_string());
                return None;
            }
        }
    }
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        Ok(f) => Some(f),
        Err(e) => {
            warn_once(warned, path, &e.to_string());
            None
        }
    }
}

fn warn_once(warned: &mut bool, path: &PathBuf, err: &str) {
    if !*warned {
        eprintln!("slag: event sink {} failed: {err}", path.display());
        *warned = true;
    }
}

/// Compact colored one-liner display on stderr — the stream-mode view of
/// the agent loop until the Ratatui dashboard lands.
pub struct StderrNarrator;

impl StderrNarrator {
    pub fn spawn_narrator(mut rx: UnboundedReceiver<EngineEvent>) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                Self::narrate(&event);
            }
        })
    }

    fn narrate(event: &EngineEvent) {
        match event {
            EngineEvent::TurnStart { turn } => {
                Self::line(HOT, "⚒", &format!("turn {turn}"));
            }
            EngineEvent::ModelCall { model } => {
                Self::line(COLD, "⚙", model);
            }
            EngineEvent::ModelRouted { routed, .. } => {
                Self::line(COLD, "⚙", &format!("routed to {routed}"));
            }
            EngineEvent::ToolCallStart { name, preview: p } => {
                Self::line(BRIGHT, "→", &format!("{name}: {}", preview(p, 80)));
            }
            EngineEvent::ToolResult { name, ok: true, .. } => {
                Self::line(PURE, "✓", &format!("{name} ok"));
            }
            EngineEvent::ToolResult { name, ok: false, preview: p } => {
                Self::line(WARM, "✗", &format!("{name}: {}", preview(p, 80)));
            }
            EngineEvent::Tokens { usage } => {
                let msg = match usage.cost {
                    Some(cost) => format!("{} tok (${cost:.4})", usage.total_tokens),
                    None => format!("{} tok", usage.total_tokens),
                };
                Self::line(COLD, "◦", &msg);
            }
            EngineEvent::Steer { text } => {
                Self::line(BRIGHT, "↪", &format!("steer: {}", preview(text, 80)));
            }
            EngineEvent::Finish { summary } => {
                Self::line(PURE, "■", &preview(summary, 120));
            }
            EngineEvent::Error { message } => {
                Self::line(WARM, "✗", &preview(message, 120));
            }
            EngineEvent::IngotStart { id, work } => {
                Self::line(HOT, "🧱", &format!("[{id}] {}", preview(work, 60)));
            }
            EngineEvent::HeatTick { id, heat } => {
                Self::line(WARM, "🔥", &format!("[{id}] heat {heat}"));
            }
            EngineEvent::IngotDone { id, ok: true } => {
                Self::line(PURE, "✅", &format!("[{id}] forged"));
            }
            EngineEvent::IngotDone { id, ok: false } => {
                Self::line(WARM, "❌", &format!("[{id}] cracked"));
            }
            EngineEvent::DuelRound { id, round } => {
                Self::line(BRIGHT, "⚔", &format!("[{id}] duel round {round}"));
            }
            EngineEvent::DuelVerdict { id, winner, margin } => {
                Self::line(PURE, "⚖", &format!("[{id}] cast {winner} wins by {margin}"));
            }
        }
    }

    fn line(color: crossterm::style::Color, icon: &str, msg: &str) {
        eprintln!("  {}{icon}{} {msg}", SetForegroundColor(color), ResetColor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Usage;

    #[test]
    fn events_serialize_with_snake_case_tags() {
        let v = serde_json::to_value(EngineEvent::TurnStart { turn: 3 }).unwrap();
        assert_eq!(v, serde_json::json!({"event": "turn_start", "turn": 3}));

        let v = serde_json::to_value(EngineEvent::ToolCallStart {
            name: "bash".into(),
            preview: "cargo test".into(),
        })
        .unwrap();
        assert_eq!(v["event"], "tool_call_start");
        assert_eq!(v["name"], "bash");

        let v = serde_json::to_value(EngineEvent::ToolResult {
            name: "edit_file".into(),
            ok: false,
            preview: "no match".into(),
        })
        .unwrap();
        assert_eq!(v["event"], "tool_result");
        assert_eq!(v["ok"], false);

        let v = serde_json::to_value(EngineEvent::Finish { summary: "done".into() }).unwrap();
        assert_eq!(v["event"], "finish");
    }

    #[test]
    fn channel_delivers_events() {
        let (tx, mut rx) = channel();
        tx.send(EngineEvent::TurnStart { turn: 1 }).unwrap();
        drop(tx);
        assert!(matches!(rx.try_recv(), Ok(EngineEvent::TurnStart { turn: 1 })));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn jsonl_sink_writes_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path also proves parent-dir creation.
        let path = dir.path().join("logs").join("events.jsonl");

        let (tx, rx) = channel();
        let handle = spawn_jsonl_sink(rx, path.clone());

        tx.send(EngineEvent::TurnStart { turn: 1 }).unwrap();
        tx.send(EngineEvent::ModelCall { model: "qwen/qwen3-coder".into() }).unwrap();
        tx.send(EngineEvent::Tokens { usage: Usage::default() }).unwrap();
        drop(tx);
        handle.await.unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v["event"].is_string());
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[1]).unwrap()["model"],
            "qwen/qwen3-coder"
        );
    }

    #[tokio::test]
    async fn jsonl_sink_survives_unwritable_path() {
        let (tx, rx) = channel();
        let handle = spawn_jsonl_sink(rx, PathBuf::from("/dev/null/impossible/events.jsonl"));
        tx.send(EngineEvent::TurnStart { turn: 1 }).unwrap();
        drop(tx);
        // Must complete without panicking despite the doomed path.
        handle.await.unwrap();
    }

    #[test]
    fn preview_truncates_on_char_boundary_with_multibyte() {
        let s = "héllo wörld ⚒⚒⚒ fire";
        let p = preview(s, 8);
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= 8);
        // Never splits a multibyte char: the result is valid UTF-8 by
        // construction, and re-slicing must not panic.
        let _ = &p[..];

        let all_multibyte = "⚒⚒⚒⚒⚒⚒";
        let p = preview(all_multibyte, 4);
        assert_eq!(p.chars().count(), 4);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn preview_collapses_whitespace_and_passes_short_input() {
        assert_eq!(preview("cargo test", 80), "cargo test");
        assert_eq!(preview("  cargo\n\ttest \n --all ", 80), "cargo test --all");
        assert_eq!(preview("anything", 0), "");
        // Exactly at the limit: no ellipsis.
        assert_eq!(preview("abcd", 4), "abcd");
    }

    #[test]
    fn preview_strips_control_chars() {
        // ANSI color/clear sequences lose their ESC and cannot re-arm.
        assert_eq!(preview("a\x1b[31mred\x1b[0m b", 80), "a[31mred[0m b");
        assert_eq!(preview("bell\x07 and \x08backspace", 80), "bell and backspace");
        // Whitespace controls (\n, \t) still collapse to single spaces.
        assert_eq!(preview("x\n\ty", 80), "x y");
    }

    #[test]
    fn narrator_handles_every_variant_without_panicking() {
        for event in [
            EngineEvent::TurnStart { turn: 3 },
            EngineEvent::ModelCall { model: "m".into() },
            EngineEvent::ToolCallStart { name: "bash".into(), preview: "cargo test".into() },
            EngineEvent::ToolResult { name: "bash".into(), ok: true, preview: "ok".into() },
            EngineEvent::ToolResult { name: "bash".into(), ok: false, preview: "boom".into() },
            EngineEvent::Tokens {
                usage: Usage { total_tokens: 42, cost: Some(0.01), ..Default::default() },
            },
            EngineEvent::Steer { text: "focus on tests".into() },
            EngineEvent::Finish { summary: "forged".into() },
            EngineEvent::Error { message: "cracked".into() },
        ] {
            StderrNarrator::narrate(&event);
        }
    }
}
