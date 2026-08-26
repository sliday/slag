//! insights — offline analytics over the slag heap (item 100).
//!
//! `slag insights` reads the per-run ledgers (`logs/run-*.jsonl`) and the
//! per-session engine event streams (every other `logs/*.jsonl`),
//! aggregates deterministic stats — ingots forged/cracked, heats, spend,
//! tokens, tool errors, duel margins — and prints one report. Each log
//! file's summary caches as `logs/facets/<stem>.json`; a facet newer than
//! its log is reused, `--refresh` recomputes everything. Offline by
//! design: no key, no network, and parsing rides the crash-tolerant
//! JSONL reader (item 80), so a garbled line never sinks the report.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::events::RunEntry;
use crate::engine::transcript::read_jsonl_tolerant;
use crate::error::SlagError;

/// Bump when `Facet` changes shape: stale-schema facets recompute.
const FACET_SCHEMA: u32 = 1;

/// Facet cache directory under the slag heap.
const FACET_DIR: &str = "logs/facets";

/// Deterministic per-log-file summary. One type covers both kinds of
/// log: ledger fields stay zero for engine streams and vice versa.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Facet {
    pub schema: u32,
    /// 1 for a run ledger, 0 for an engine event stream.
    pub runs: u32,
    pub forged: u32,
    pub cracked: u32,
    /// Final heat of each finished ingot, in ledger order.
    pub heats: Vec<u8>,
    pub assay_ok: Option<bool>,
    pub model: Option<String>,
    pub started: Option<String>,
    pub cost: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Failed tool dispatches by tool name.
    pub tool_errors: BTreeMap<String, u64>,
    /// Assayer margin of each duel verdict.
    pub duel_margins: Vec<u8>,
}

impl Facet {
    fn absorb(&mut self, other: &Facet) {
        self.runs += other.runs;
        self.forged += other.forged;
        self.cracked += other.cracked;
        self.heats.extend_from_slice(&other.heats);
        self.cost += other.cost;
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        for (name, n) in &other.tool_errors {
            *self.tool_errors.entry(name.clone()).or_insert(0) += n;
        }
        self.duel_margins.extend_from_slice(&other.duel_margins);
    }
}

/// One row per run ledger for the report's run table.
#[derive(Debug, Clone)]
pub struct RunRow {
    pub stem: String,
    pub facet: Facet,
}

/// Everything the report prints.
#[derive(Debug, Default)]
pub struct Report {
    pub totals: Facet,
    pub runs: Vec<RunRow>,
    /// Assay verdicts seen: (passed, failed).
    pub assays: (u32, u32),
}

/// Aggregate every `logs/*.jsonl` under `root`, using cached facets
/// unless `refresh`.
pub fn gather(root: &Path, refresh: bool) -> Report {
    let log_dir = root.join(crate::config::LOG_DIR);
    let facet_dir = root.join(FACET_DIR);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&log_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    paths.sort();

    let mut report = Report { totals: Facet { schema: FACET_SCHEMA, ..Facet::default() }, ..Report::default() };
    for path in paths {
        let facet = facet_for(&path, &facet_dir, refresh);
        report.totals.absorb(&facet);
        match facet.assay_ok {
            Some(true) => report.assays.0 += 1,
            Some(false) => report.assays.1 += 1,
            None => {}
        }
        if facet.runs > 0 {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            report.runs.push(RunRow { stem, facet });
        }
    }
    report
}

/// Cached facet for one log file: reuse when the facet is current
/// (schema matches, at least as new as the log), else recompute and
/// rewrite. Cache IO is best-effort — a read-only heap still reports.
fn facet_for(path: &Path, facet_dir: &Path, refresh: bool) -> Facet {
    let cache = path
        .file_stem()
        .map(|stem| facet_dir.join(format!("{}.json", stem.to_string_lossy())));
    if !refresh {
        if let Some(cache) = &cache {
            if let Some(facet) = load_fresh_facet(cache, path) {
                return facet;
            }
        }
    }
    let facet = compute_facet(path);
    if let Some(cache) = &cache {
        let _ = std::fs::create_dir_all(facet_dir);
        if let Ok(json) = serde_json::to_string_pretty(&facet) {
            let _ = std::fs::write(cache, json);
        }
    }
    facet
}

fn load_fresh_facet(cache: &Path, log: &Path) -> Option<Facet> {
    let cache_mtime = std::fs::metadata(cache).and_then(|m| m.modified()).ok()?;
    let log_mtime = std::fs::metadata(log).and_then(|m| m.modified()).ok()?;
    if cache_mtime < log_mtime {
        return None;
    }
    let facet: Facet = serde_json::from_str(&std::fs::read_to_string(cache).ok()?).ok()?;
    (facet.schema == FACET_SCHEMA).then_some(facet)
}

fn compute_facet(path: &Path) -> Facet {
    let is_ledger = path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with("run-"));
    let mut facet = Facet { schema: FACET_SCHEMA, ..Facet::default() };
    if is_ledger {
        facet.runs = 1;
        for entry in read_jsonl_tolerant::<RunEntry>(path) {
            match entry {
                RunEntry::RunMeta { model, started, .. } => {
                    facet.model = Some(model);
                    facet.started = Some(started);
                }
                RunEntry::IngotDone { ok, heat, .. } => {
                    if ok {
                        facet.forged += 1;
                    } else {
                        facet.cracked += 1;
                    }
                    facet.heats.push(heat);
                }
                RunEntry::Assay { ok, .. } => facet.assay_ok = Some(ok),
                RunEntry::Note { .. } => {}
            }
        }
    } else {
        for v in read_jsonl_tolerant::<Value>(path) {
            match v.get("event").and_then(Value::as_str) {
                Some("tokens") => {
                    let usage = v.get("usage");
                    let num = |key: &str| {
                        usage.and_then(|u| u.get(key)).and_then(Value::as_u64).unwrap_or(0)
                    };
                    facet.prompt_tokens += num("prompt_tokens");
                    facet.completion_tokens += num("completion_tokens");
                    if let Some(c) =
                        usage.and_then(|u| u.get("cost")).and_then(Value::as_f64)
                    {
                        facet.cost += c;
                    }
                }
                Some("tool_result") => {
                    if v.get("ok").and_then(Value::as_bool) == Some(false) {
                        let name = v
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        *facet.tool_errors.entry(name).or_insert(0) += 1;
                    }
                }
                Some("duel_verdict") => {
                    if let Some(margin) = v.get("margin").and_then(Value::as_u64) {
                        facet.duel_margins.push(margin.min(u8::MAX as u64) as u8);
                    }
                }
                _ => {}
            }
        }
    }
    facet
}

/// `slag insights`: aggregate and print. Never demands a key — like
/// `status` and `runs`, it inspects state that already exists.
pub fn run(root: &Path, refresh: bool) -> Result<(), SlagError> {
    let report = gather(root, refresh);
    let t = &report.totals;
    println!("\n  INSIGHTS ({}/*.jsonl)\n", crate::config::LOG_DIR);
    if t.runs == 0 && t.prompt_tokens == 0 && t.forged + t.cracked == 0 {
        println!("  nothing to analyze yet — forge something first\n");
        return Ok(());
    }

    println!(
        "  runs         {} ledgers ({} assay-pass, {} assay-fail)",
        t.runs, report.assays.0, report.assays.1
    );
    let done = t.forged + t.cracked;
    let crack_pct = if done > 0 { t.cracked as f64 * 100.0 / done as f64 } else { 0.0 };
    println!(
        "  ingots       {} forged · {} cracked ({crack_pct:.0}% crack rate)",
        t.forged, t.cracked
    );
    if !t.heats.is_empty() {
        let sum: u64 = t.heats.iter().map(|h| *h as u64).sum();
        let max = t.heats.iter().max().copied().unwrap_or(0);
        println!(
            "  heats        avg {:.1} · max {max}",
            sum as f64 / t.heats.len() as f64
        );
    }
    println!(
        "  spend        ${:.2} · {} prompt + {} completion tokens",
        t.cost, t.prompt_tokens, t.completion_tokens
    );
    let errors: u64 = t.tool_errors.values().sum();
    if errors > 0 {
        let mut by_tool: Vec<(&String, &u64)> = t.tool_errors.iter().collect();
        by_tool.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let detail: Vec<String> =
            by_tool.iter().take(5).map(|(n, c)| format!("{n} {c}")).collect();
        println!("  tool errors  {errors} ({})", detail.join(", "));
    } else {
        println!("  tool errors  0");
    }
    if t.duel_margins.is_empty() {
        println!("  duels        n/a (no duel verdicts in the logs)");
    } else {
        let sum: u64 = t.duel_margins.iter().map(|m| *m as u64).sum();
        println!(
            "  duels        {} verdicts · avg margin {:.1}",
            t.duel_margins.len(),
            sum as f64 / t.duel_margins.len() as f64
        );
    }

    if !report.runs.is_empty() {
        println!("\n  per run:");
        for row in report.runs.iter().rev().take(10) {
            let f = &row.facet;
            let verdict = match f.assay_ok {
                Some(true) => "pass",
                Some(false) => "fail",
                None => "—",
            };
            println!(
                "    {}  {} forged · {} cracked · {} [{}]",
                row.stem,
                f.forged,
                f.cracked,
                f.model.as_deref().unwrap_or("?"),
                verdict
            );
        }
    }
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ledger(dir: &Path, name: &str) {
        let lines = [
            serde_json::to_string(&RunEntry::RunMeta {
                run_id: "r1".into(),
                started: "2026-08-26T00:00:00Z".into(),
                git_branch: None,
                model: "openrouter/auto".into(),
                duel: "auto".into(),
                flux_profile: "bare".into(),
                crucible_hash: None,
            })
            .unwrap(),
            serde_json::to_string(&RunEntry::IngotDone { id: "i1".into(), ok: true, heat: 1 })
                .unwrap(),
            serde_json::to_string(&RunEntry::IngotDone { id: "i2".into(), ok: false, heat: 5 })
                .unwrap(),
            "this line is not json".into(),
            serde_json::to_string(&RunEntry::Assay { total: 2, forged: 1, cracked: 1, ok: false })
                .unwrap(),
        ];
        // Truncated tail: a partial write the tolerant reader must drop.
        let content = format!("{}\n{{\"entry\":\"ingot_do", lines.join("\n"));
        std::fs::write(dir.join(name), content).unwrap();
    }

    fn write_engine_log(dir: &Path, name: &str) {
        let content = concat!(
            "{\"event\":\"tokens\",\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":40,\"total_tokens\":140,\"cost\":0.25}}\n",
            "{\"event\":\"tokens\",\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":10,\"total_tokens\":60,\"cost\":0.05}}\n",
            "{\"event\":\"tool_result\",\"name\":\"bash\",\"ok\":false,\"preview\":\"boom\"}\n",
            "{\"event\":\"tool_result\",\"name\":\"bash\",\"ok\":true,\"preview\":\"fine\"}\n",
            "{\"event\":\"tool_result\",\"name\":\"edit_file\",\"ok\":false,\"preview\":\"no match\"}\n",
            "{\"event\":\"duel_verdict\",\"id\":\"i1\",\"winner\":\"a\",\"margin\":3}\n",
            "garbage\n",
        );
        std::fs::write(dir.join(name), content).unwrap();
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join(crate::config::LOG_DIR);
        std::fs::create_dir_all(&logs).unwrap();
        (dir, logs)
    }

    #[test]
    fn aggregates_ledgers_and_engine_events_despite_bad_lines() {
        let (dir, logs) = setup();
        write_ledger(&logs, "run-20260826-1.jsonl");
        write_engine_log(&logs, "engine-20260826_010203.jsonl");

        let report = gather(dir.path(), false);
        let t = &report.totals;
        assert_eq!(t.runs, 1);
        assert_eq!((t.forged, t.cracked), (1, 1));
        assert_eq!(t.heats, vec![1, 5]);
        assert_eq!(report.assays, (0, 1));
        assert!((t.cost - 0.30).abs() < 1e-9, "{}", t.cost);
        assert_eq!(t.prompt_tokens, 150);
        assert_eq!(t.completion_tokens, 50);
        assert_eq!(t.tool_errors.get("bash"), Some(&1));
        assert_eq!(t.tool_errors.get("edit_file"), Some(&1));
        assert_eq!(t.duel_margins, vec![3]);
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.runs[0].facet.model.as_deref(), Some("openrouter/auto"));

        // Facets cached, one per log file.
        let facets = dir.path().join(FACET_DIR);
        assert!(facets.join("run-20260826-1.json").exists());
        assert!(facets.join("engine-20260826_010203.json").exists());
    }

    #[test]
    fn facet_cache_is_reused_until_refresh() {
        let (dir, logs) = setup();
        write_ledger(&logs, "run-20260826-2.jsonl");
        gather(dir.path(), false);

        // Tamper with the cached facet: a cache hit shows the tampered
        // numbers, --refresh recomputes the truth.
        let cache = dir.path().join(FACET_DIR).join("run-20260826-2.json");
        let mut facet: Facet =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        facet.forged = 99;
        std::fs::write(&cache, serde_json::to_string(&facet).unwrap()).unwrap();

        assert_eq!(gather(dir.path(), false).totals.forged, 99, "cache must be reused");
        assert_eq!(gather(dir.path(), true).totals.forged, 1, "--refresh recomputes");
        let rewritten: Facet =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(rewritten.forged, 1, "refresh rewrites the cache");
    }

    #[test]
    fn stale_schema_facets_recompute() {
        let (dir, logs) = setup();
        write_ledger(&logs, "run-20260826-3.jsonl");
        gather(dir.path(), false);
        let cache = dir.path().join(FACET_DIR).join("run-20260826-3.json");
        let mut facet: Facet =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        facet.schema = 0;
        facet.forged = 99;
        std::fs::write(&cache, serde_json::to_string(&facet).unwrap()).unwrap();
        assert_eq!(
            gather(dir.path(), false).totals.forged,
            1,
            "old-schema facet must not be trusted"
        );
    }

    #[test]
    fn empty_heap_reports_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let report = gather(dir.path(), false);
        assert_eq!(report.totals.runs, 0);
        assert!(run(dir.path(), false).is_ok());
    }
}
