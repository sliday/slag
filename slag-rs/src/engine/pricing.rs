//! Local pricing table and cost ledger.
//!
//! OpenRouter only reports `usage.cost` when the request asks for it, and
//! proxies in front of it strip the field routinely. Item 34 fills the gap:
//! prices come from the same `GET /models` fetch that already feeds the
//! context-window cache, get cached on disk for a day, and any cost derived
//! from them is flagged `estimated` so readouts can say `~$0.0123 (est)`
//! rather than pretending the provider said so.
//!
//! Item 35 keys the ledger on `(model, role)`, so judge and duel spend stop
//! hiding inside one session total.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Role, Usage};

/// A day. Model prices move rarely, and a stale price only skews an
/// estimate that is already labelled as one.
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// USD per token, the unit OpenRouter publishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub prompt: f64,
    pub completion: f64,
}

impl Pricing {
    /// Cost of one call. Prompt and completion tokens price separately;
    /// `total_tokens` is ignored because it double-counts.
    pub fn cost_of(&self, usage: &Usage) -> f64 {
        usage.prompt_tokens as f64 * self.prompt + usage.completion_tokens as f64 * self.completion
    }
}

/// Model id → price, with the variant-suffix fallback the window cache
/// already uses (`qwen/qwen3-coder:nitro` → `qwen/qwen3-coder`). Windows
/// carry across every suffix; prices do not carry across `:free`, whose
/// whole point is that the variant bills nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PricingTable {
    #[serde(default)]
    pub models: HashMap<String, Pricing>,
    /// Unix seconds the table was fetched, for the disk cache TTL.
    #[serde(default)]
    pub fetched_at: u64,
}

impl PricingTable {
    pub fn lookup(&self, model: &str) -> Option<Pricing> {
        if let Some(p) = self.models.get(model) {
            return Some(*p);
        }
        // A free variant OpenRouter did not list is still free. Charging it
        // the base model's rate invents a bill nobody owes.
        if model.ends_with(":free") {
            return None;
        }
        let bare = model.split(':').next().unwrap_or(model);
        self.models.get(bare).copied()
    }

    /// Estimated USD for `usage` under `model`. `None` when the model is
    /// unknown or priced at zero on both legs (a free model's real cost is
    /// zero, and reporting `$0.0000 (est)` reads as a bug).
    pub fn estimate(&self, model: &str, usage: &Usage) -> Option<f64> {
        let p = self.lookup(model)?;
        if p.prompt == 0.0 && p.completion == 0.0 {
            return None;
        }
        Some(p.cost_of(usage))
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    fn expired(&self, now: u64) -> bool {
        now.saturating_sub(self.fetched_at) > CACHE_TTL_SECS
    }
}

/// Parse the `GET /models` body into a price table. Prices arrive as
/// strings ("0.0000004"), sometimes as numbers, sometimes as "-1" for
/// models whose price is variable — anything that does not parse to a
/// non-negative float is dropped rather than guessed at.
pub fn parse_table(body: &str) -> PricingTable {
    let root: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return PricingTable::default(),
    };
    let mut models = HashMap::new();
    let entries = root.get("data").and_then(|d| d.as_array());
    for entry in entries.into_iter().flatten() {
        let Some(id) = entry.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let Some(p) = entry.get("pricing") else { continue };
        let prompt = lenient_price(p.get("prompt"));
        let completion = lenient_price(p.get("completion"));
        if let (Some(prompt), Some(completion)) = (prompt, completion) {
            models.insert(id.to_string(), Pricing { prompt, completion });
        }
    }
    PricingTable {
        models,
        fetched_at: now_secs(),
    }
}

fn lenient_price(v: Option<&serde_json::Value>) -> Option<f64> {
    let n = match v? {
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        serde_json::Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (n.is_finite() && n >= 0.0).then_some(n)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path() -> Option<PathBuf> {
    crate::config::config_dir_path().map(|d| d.join("pricing.json"))
}

/// Read the disk cache, or `None` when it is missing, unreadable, or older
/// than a day.
pub fn load_cached() -> Option<PricingTable> {
    let raw = std::fs::read_to_string(cache_path()?).ok()?;
    let table: PricingTable = serde_json::from_str(&raw).ok()?;
    (!table.is_empty() && !table.expired(now_secs())).then_some(table)
}

/// Best-effort write; a cache miss costs one extra fetch, never a run.
pub fn store(table: &PricingTable) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string(table) {
        let _ = std::fs::write(path, body);
    }
}

/// `$0.0123`, or `~$0.0123 (est)` when the number came from the local
/// table. `None` cost renders as a dash so a column stays aligned.
pub fn format_cost(usage: &Usage) -> String {
    match usage.cost {
        Some(c) if usage.estimated => format!("~${c:.4} (est)"),
        Some(c) => format!("${c:.4}"),
        None => "-".to_string(),
    }
}

/// One ledger row: what a single (model, role) pair spent.
#[derive(Debug, Clone)]
pub struct LedgerRow {
    pub model: String,
    pub role: Role,
    pub usage: Usage,
}

impl LedgerRow {
    pub fn cost(&self) -> f64 {
        self.usage.cost.unwrap_or(0.0)
    }
}

/// Spend split by model and call site (item 35). Folds `Usage` values that
/// carry their own `model`/`role` provenance; a `Usage` missing either is
/// still counted, under `unknown`/`Smith`, so no spend disappears.
#[derive(Debug, Clone, Default)]
pub struct CostLedger {
    rows: BTreeMap<(String, Role), Usage>,
}

impl CostLedger {
    /// `const` so the run-wide ledger can live in a `static Mutex`
    /// alongside the rest of `engine::stats`.
    pub const fn new() -> Self {
        Self { rows: BTreeMap::new() }
    }

    pub fn fold(&mut self, usage: &Usage) {
        let model = usage.model.clone().unwrap_or_else(|| "unknown".to_string());
        let role = usage.role.unwrap_or(Role::Smith);
        self.rows.entry((model, role)).or_default().add(usage);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Rows sorted by cost descending, then by model then role so a run
    /// with no cost data still prints in a stable order.
    pub fn rows(&self) -> Vec<LedgerRow> {
        let mut out: Vec<LedgerRow> = self
            .rows
            .iter()
            .map(|((model, role), usage)| LedgerRow {
                model: model.clone(),
                role: *role,
                usage: usage.clone(),
            })
            .collect();
        out.sort_by(|a, b| {
            b.cost()
                .partial_cmp(&a.cost())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.role.label().cmp(b.role.label()))
        });
        out
    }

    /// Everything folded so far, as one `Usage`.
    pub fn total(&self) -> Usage {
        let mut total = Usage::default();
        for usage in self.rows.values() {
            total.add(usage);
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: u64, completion: u64) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            ..Default::default()
        }
    }

    const BODY: &str = r#"{"data":[
        {"id":"qwen/qwen3-coder","pricing":{"prompt":"0.0000004","completion":"0.0000016"}},
        {"id":"free/model","pricing":{"prompt":"0","completion":"0"}},
        {"id":"variable/model","pricing":{"prompt":"-1","completion":"-1"}},
        {"id":"numeric/model","pricing":{"prompt":0.000002,"completion":0.000008}},
        {"id":"no-pricing/model"}
    ]}"#;

    #[test]
    fn parse_table_reads_string_and_numeric_prices_and_drops_the_rest() {
        let table = parse_table(BODY);
        assert_eq!(
            table.lookup("qwen/qwen3-coder"),
            Some(Pricing {
                prompt: 0.0000004,
                completion: 0.0000016
            })
        );
        assert_eq!(
            table.lookup("numeric/model"),
            Some(Pricing {
                prompt: 0.000002,
                completion: 0.000008
            })
        );
        // "-1" means variable pricing, not free: dropped, not stored as -1.
        assert_eq!(table.lookup("variable/model"), None);
        assert_eq!(table.lookup("no-pricing/model"), None);
        assert!(!table.is_empty());
    }

    #[test]
    fn lookup_falls_back_from_variant_suffix_to_the_bare_id() {
        let table = parse_table(BODY);
        assert_eq!(
            table.lookup("qwen/qwen3-coder:nitro").map(|p| p.prompt),
            Some(0.0000004),
            "a routing variant shares the base model's price"
        );
        assert_eq!(table.lookup("mystery/model:nitro"), None);
    }

    /// `:free` is the one suffix that does not share the base price: the
    /// whole point of the variant is that it costs nothing. Guessing the
    /// paid rate for an unlisted free model prints a fabricated bill.
    #[test]
    fn a_free_variant_never_inherits_the_paid_base_price() {
        let table = parse_table(BODY);
        assert_eq!(
            table.lookup("qwen/qwen3-coder:free"),
            None,
            "an unlisted free variant is not priced at the paid base rate"
        );
        assert_eq!(
            table.estimate("qwen/qwen3-coder:free", &usage(1_000_000, 100_000)),
            None,
            "a free call bills nothing, not $0.56"
        );
    }

    /// When OpenRouter does list the free variant, its own zero price
    /// answers first and the suffix rule never comes up.
    #[test]
    fn an_explicitly_listed_free_variant_still_reads_as_free() {
        let table = parse_table(
            r#"{"data":[
                {"id":"a/model","pricing":{"prompt":"0.000001","completion":"0.000002"}},
                {"id":"a/model:free","pricing":{"prompt":"0","completion":"0"}}
            ]}"#,
        );
        assert_eq!(table.lookup("a/model:free"), Some(Pricing::default()));
        assert_eq!(table.estimate("a/model:free", &usage(1_000, 1_000)), None);
    }

    #[test]
    fn estimate_prices_prompt_and_completion_separately() {
        let table = parse_table(BODY);
        let cost = table
            .estimate("qwen/qwen3-coder", &usage(1_000_000, 100_000))
            .expect("priced model");
        // 1M prompt at 4e-7 = $0.40, 100k completion at 1.6e-6 = $0.16.
        assert!((cost - 0.56).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn a_free_model_estimates_to_nothing_rather_than_a_zero_dollar_line() {
        let table = parse_table(BODY);
        assert_eq!(table.estimate("free/model", &usage(1_000, 1_000)), None);
    }

    #[test]
    fn a_malformed_body_yields_an_empty_table_instead_of_panicking() {
        let table = parse_table("not json at all");
        assert!(table.is_empty());
        assert_eq!(table.estimate("qwen/qwen3-coder", &usage(10, 10)), None);
    }

    #[test]
    fn an_expired_cache_is_refused() {
        let fresh = PricingTable {
            models: HashMap::new(),
            fetched_at: 1_000_000,
        };
        assert!(!fresh.expired(1_000_000 + CACHE_TTL_SECS));
        assert!(fresh.expired(1_000_000 + CACHE_TTL_SECS + 1));
    }

    #[test]
    fn format_cost_marks_estimates_and_dashes_a_missing_cost() {
        let mut u = usage(10, 10);
        u.cost = Some(0.0123);
        assert_eq!(format_cost(&u), "$0.0123");
        u.estimated = true;
        assert_eq!(format_cost(&u), "~$0.0123 (est)");
        u.cost = None;
        assert_eq!(format_cost(&u), "-");
    }

    #[test]
    fn adding_an_estimated_leg_taints_the_sum() {
        let mut total = usage(10, 10);
        total.cost = Some(1.0);
        let mut est = usage(5, 5);
        est.cost = Some(0.5);
        est.estimated = true;
        total.add(&est);
        assert!(total.estimated, "one estimated leg makes the sum an estimate");
        assert_eq!(total.cost, Some(1.5));
        assert_eq!(total.prompt_tokens, 15);
    }

    fn attributed(model: &str, role: Role, cost: f64) -> Usage {
        Usage {
            prompt_tokens: 100,
            completion_tokens: 10,
            total_tokens: 110,
            cost: Some(cost),
            estimated: false,
            model: Some(model.to_string()),
            role: Some(role),
        }
    }

    #[test]
    fn ledger_splits_spend_per_model_and_role() {
        let mut ledger = CostLedger::new();
        ledger.fold(&attributed("a/model", Role::Smith, 1.0));
        ledger.fold(&attributed("a/model", Role::Smith, 0.5));
        ledger.fold(&attributed("a/model", Role::Judge, 0.25));
        ledger.fold(&attributed("b/model", Role::Founder, 2.0));

        let rows = ledger.rows();
        assert_eq!(rows.len(), 3, "one row per (model, role) pair");
        // Sorted by cost descending.
        assert_eq!(rows[0].model, "b/model");
        assert_eq!(rows[0].role, Role::Founder);
        assert_eq!(rows[1].model, "a/model");
        assert_eq!(rows[1].role, Role::Smith);
        assert!((rows[1].cost() - 1.5).abs() < 1e-9, "same-key calls fold");
        assert_eq!(rows[2].role, Role::Judge, "judge spend is visible on its own row");
        assert!((ledger.total().cost.unwrap() - 3.75).abs() < 1e-9);
    }

    #[test]
    fn unattributed_usage_still_counts_instead_of_vanishing() {
        let mut ledger = CostLedger::new();
        let mut anon = usage(10, 10);
        anon.cost = Some(0.75);
        ledger.fold(&anon);
        let rows = ledger.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "unknown");
        assert_eq!(rows[0].role, Role::Smith);
        assert!((ledger.total().cost.unwrap() - 0.75).abs() < 1e-9);
    }
}
