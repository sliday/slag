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

/// Published list rates in USD per million tokens. Snapshot date: 2026-10-15.
#[derive(Debug, Clone, Copy)]
pub struct ListPrice {
    pub provider: &'static str,
    pub model: &'static str,
    pub input: f64,
    pub output: f64,
    pub cached_input: Option<f64>,
    pub date: &'static str,
    pub source: &'static str,
}

impl ListPrice {
    pub fn cost_of(&self, usage: &Usage, cached_tokens: u64) -> f64 {
        let cached = cached_tokens.min(usage.prompt_tokens);
        let mut input = self.input;
        let mut output = self.output;
        let mut cached_input = self.cached_input.unwrap_or(input);
        if self.provider == "xai" && usage.prompt_tokens >= 200_000 {
            input *= 2.0;
            output *= 2.0;
            cached_input *= 2.0;
        }
        ((usage.prompt_tokens - cached) as f64 * input
            + cached as f64 * cached_input
            + usage.completion_tokens as f64 * output) / 1_000_000.0
    }
}

// Official list prices, checked 2026-10-15. Estimates exclude taxes and discounts.
// DeepSeek uses peak rates. Gemini latest uses the current promotional rate.
pub const LIST_PRICES: &[ListPrice] = &[
    ListPrice { provider: "openai", model: "gpt-4.1-mini", input: 0.4, output: 1.6, cached_input: Some(0.1), date: "2026-10-15", source: "https://developers.openai.com/api/docs/pricing" },
    ListPrice { provider: "openai", model: "gpt-4.1-mini-2025-04-14", input: 0.4, output: 1.6, cached_input: Some(0.1), date: "2026-10-15", source: "https://developers.openai.com/api/docs/pricing" },
    ListPrice { provider: "openai", model: "gpt-4.1", input: 2.0, output: 8.0, cached_input: Some(0.5), date: "2026-10-15", source: "https://developers.openai.com/api/docs/pricing" },
    ListPrice { provider: "openai", model: "gpt-4o-mini", input: 0.15, output: 0.6, cached_input: Some(0.075), date: "2026-10-15", source: "https://developers.openai.com/api/docs/pricing" },
    ListPrice { provider: "openai", model: "gpt-5-mini", input: 0.25, output: 2.0, cached_input: Some(0.025), date: "2026-10-15", source: "https://developers.openai.com/api/docs/pricing" },
    ListPrice { provider: "anthropic", model: "claude-sonnet-4-5", input: 3.0, output: 15.0, cached_input: Some(0.3), date: "2026-10-15", source: "https://platform.claude.com/docs/en/about-claude/pricing" },
    ListPrice { provider: "anthropic", model: "claude-sonnet-4-6", input: 3.0, output: 15.0, cached_input: Some(0.3), date: "2026-10-15", source: "https://platform.claude.com/docs/en/about-claude/pricing" },
    ListPrice { provider: "anthropic", model: "claude-haiku-4-5", input: 1.0, output: 5.0, cached_input: Some(0.1), date: "2026-10-15", source: "https://platform.claude.com/docs/en/about-claude/pricing" },
    ListPrice { provider: "anthropic", model: "claude-opus-4-6", input: 5.0, output: 25.0, cached_input: Some(0.5), date: "2026-10-15", source: "https://platform.claude.com/docs/en/about-claude/pricing" },
    ListPrice { provider: "gemini", model: "gemini-flash-latest", input: 0.75, output: 3.75, cached_input: Some(0.075), date: "2026-10-15", source: "https://ai.google.dev/gemini-api/docs/pricing" },
    ListPrice { provider: "gemini", model: "gemini-2.5-flash", input: 0.3, output: 2.5, cached_input: Some(0.03), date: "2026-10-15", source: "https://ai.google.dev/gemini-api/docs/pricing" },
    ListPrice { provider: "gemini", model: "gemini-2.5-flash-lite", input: 0.1, output: 0.4, cached_input: Some(0.01), date: "2026-10-15", source: "https://ai.google.dev/gemini-api/docs/pricing" },
    ListPrice { provider: "gemini", model: "gemini-3.1-flash-lite", input: 0.25, output: 1.5, cached_input: Some(0.025), date: "2026-10-15", source: "https://ai.google.dev/gemini-api/docs/pricing" },
    ListPrice { provider: "groq", model: "openai/gpt-oss-120b", input: 0.15, output: 0.6, cached_input: Some(0.075), date: "2026-10-15", source: "https://console.groq.com/docs/models" },
    ListPrice { provider: "groq", model: "openai/gpt-oss-20b", input: 0.075, output: 0.3, cached_input: Some(0.037), date: "2026-10-15", source: "https://console.groq.com/docs/models" },
    ListPrice { provider: "cerebras", model: "qwen-3.8-27b", input: 0.99, output: 1.49, cached_input: None, date: "2026-10-15", source: "https://www.cerebras.ai/pricing" },
    ListPrice { provider: "cerebras", model: "gpt-oss-120b", input: 0.35, output: 0.75, cached_input: None, date: "2026-10-15", source: "https://www.cerebras.ai/pricing" },
    ListPrice { provider: "xai", model: "grok-4.3", input: 1.25, output: 2.5, cached_input: Some(0.2), date: "2026-10-15", source: "https://docs.x.ai/developers/pricing" },
    ListPrice { provider: "xai", model: "grok-4.5", input: 2.0, output: 6.0, cached_input: Some(0.3), date: "2026-10-15", source: "https://docs.x.ai/developers/pricing" },
    ListPrice { provider: "xai", model: "grok-4.6", input: 2.0, output: 6.0, cached_input: Some(0.5), date: "2026-10-15", source: "https://docs.x.ai/developers/pricing" },
    ListPrice { provider: "mistral", model: "mistral-small-latest", input: 0.15, output: 0.6, cached_input: Some(0.015), date: "2026-10-15", source: "https://docs.mistral.ai/inference/pricing" },
    ListPrice { provider: "mistral", model: "mistral-small-2603", input: 0.15, output: 0.6, cached_input: Some(0.015), date: "2026-10-15", source: "https://docs.mistral.ai/inference/pricing" },
    ListPrice { provider: "mistral", model: "mistral-large-2512", input: 0.5, output: 1.5, cached_input: Some(0.05), date: "2026-10-15", source: "https://docs.mistral.ai/inference/pricing" },
    ListPrice { provider: "mistral", model: "codestral-2508", input: 0.3, output: 0.9, cached_input: Some(0.03), date: "2026-10-15", source: "https://docs.mistral.ai/inference/pricing" },
    ListPrice { provider: "deepseek", model: "deepseek-flash", input: 0.3, output: 1.2, cached_input: Some(0.006), date: "2026-10-15", source: "https://api-docs.deepseek.com/quick_start/pricing/" },
    ListPrice { provider: "deepseek", model: "deepseek-v4-flash", input: 0.3, output: 1.2, cached_input: Some(0.006), date: "2026-10-15", source: "https://api-docs.deepseek.com/quick_start/pricing/" },
    ListPrice { provider: "deepseek", model: "deepseek-v4-pro", input: 1.32, output: 3.96, cached_input: Some(0.044), date: "2026-10-15", source: "https://api-docs.deepseek.com/quick_start/pricing/" },
    ListPrice { provider: "together", model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", input: 1.04, output: 1.04, cached_input: None, date: "2026-10-15", source: "https://www.together.ai/pricing" },
    ListPrice { provider: "together", model: "openai/gpt-oss-120b", input: 0.15, output: 0.6, cached_input: None, date: "2026-10-15", source: "https://www.together.ai/pricing" },
    ListPrice { provider: "together", model: "Qwen/Qwen2.5-7B-Instruct-Turbo", input: 0.3, output: 0.3, cached_input: None, date: "2026-10-15", source: "https://www.together.ai/pricing" },
    ListPrice { provider: "fireworks", model: "accounts/fireworks/models/glm-5p3-flash", input: 0.15, output: 0.5, cached_input: Some(0.03), date: "2026-10-15", source: "https://docs.fireworks.ai/serverless/pricing" },
    ListPrice { provider: "fireworks", model: "accounts/fireworks/models/glm-5p3", input: 1.4, output: 4.4, cached_input: Some(0.26), date: "2026-10-15", source: "https://docs.fireworks.ai/serverless/pricing" },
    ListPrice { provider: "fireworks", model: "accounts/fireworks/models/gpt-oss-120b", input: 0.15, output: 0.6, cached_input: Some(0.015), date: "2026-10-15", source: "https://docs.fireworks.ai/serverless/pricing" },
];

pub fn builtin_price(provider: crate::config::ProviderPreset, model: &str) -> Option<ListPrice> {
    LIST_PRICES.iter().find(|price| price.provider == provider.name() && price.model == model).copied()
}

pub fn override_price(provider: crate::config::ProviderPreset, model: &str) -> Option<ListPrice> {
    let overrides = crate::config::pricing_overrides();
    price_override(&overrides, provider, model)
}

fn price_override(
    overrides: &HashMap<String, (f64, f64, Option<f64>)>,
    provider: crate::config::ProviderPreset,
    model: &str,
) -> Option<ListPrice> {
    let &(input, output, cached_input) = overrides.get(&format!("{}::{model}", provider.name()))?;
    Some(ListPrice { provider: "override", model: "override", input, output, cached_input,
        date: "user override", source: "config.toml [pricing]" })
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

/// Show exact spend, a labelled estimate, or the tokens behind an unknown cost.
pub fn format_cost(usage: &Usage) -> String {
    match usage.cost {
        Some(c) if usage.estimated => format!("≈ ${c:.4} est."),
        Some(c) => format!("${c:.4}"),
        None if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens > 0 =>
            format!("spend unknown · {} tokens", token_count(usage.total_tokens)),
        None => format!("spend unknown · {} in / {} out tokens",
            token_count(usage.prompt_tokens), token_count(usage.completion_tokens)),
    }
}

fn token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 { format!("{:.1}M", tokens as f64 / 1_000_000.0) }
    else if tokens >= 1_000 { format!("{:.1}k", tokens as f64 / 1_000.0) }
    else { tokens.to_string() }
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
    fn format_cost_marks_estimates_and_shows_tokens_for_unknown_cost() {
        let mut u = usage(10, 10);
        u.cost = Some(0.0123);
        assert_eq!(format_cost(&u), "$0.0123");
        u.estimated = true;
        assert_eq!(format_cost(&u), "≈ $0.0123 est.");
        u.cost = None;
        assert_eq!(format_cost(&u), "spend unknown · 10 in / 10 out tokens");
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

    #[test]
    fn builtin_defaults_have_dated_provider_scoped_prices() {
        use crate::config::ProviderPreset;
        for provider in [ProviderPreset::OpenAI, ProviderPreset::Anthropic, ProviderPreset::Gemini,
            ProviderPreset::Groq, ProviderPreset::Cerebras, ProviderPreset::Xai, ProviderPreset::Mistral,
            ProviderPreset::DeepSeek, ProviderPreset::Together, ProviderPreset::Fireworks] {
            let price = builtin_price(provider, provider.default_model()).unwrap();
            assert_eq!(price.date, "2026-10-15");
            assert!(price.source.starts_with("https://"));
            assert!(LIST_PRICES.iter().filter(|price| price.provider == provider.name()).count() >= 2);
        }
        assert!(builtin_price(ProviderPreset::OpenRouter, "gpt-4.1-mini").is_none());
        assert!(builtin_price(ProviderPreset::Cerebras, "gpt-4.1-mini").is_none());
        assert!(builtin_price(ProviderPreset::Ollama, "qwen3:8b").is_none());
    }

    #[test]
    fn configured_prices_override_list_prices_and_accept_zero() {
        use crate::config::ProviderPreset;
        let overrides = HashMap::from([("openai::gpt-4.1-mini".into(), (0.0, 0.0, Some(0.0)))]);
        let price = price_override(&overrides, ProviderPreset::OpenAI, "gpt-4.1-mini")
            .or_else(|| builtin_price(ProviderPreset::OpenAI, "gpt-4.1-mini")).unwrap();
        assert_eq!(price.cost_of(&usage(1_000_000, 100_000), 0), 0.0);
        assert!(price_override(&overrides, ProviderPreset::Cerebras, "gpt-4.1-mini").is_none());
    }

    #[test]
    fn cached_input_uses_the_published_discount_without_double_counting() {
        let price = builtin_price(crate::config::ProviderPreset::OpenAI, "gpt-4.1-mini").unwrap();
        let cost = price.cost_of(&usage(1_000_000, 100_000), 250_000);
        assert!((cost - 0.485).abs() < 1e-9, "{cost}");
        assert!((price.cost_of(&usage(10, 0), 100) - 0.000001).abs() < 1e-9);
        let price = builtin_price(crate::config::ProviderPreset::Xai, "grok-4.3").unwrap();
        assert!((price.cost_of(&usage(200_000, 10_000), 0) - 0.55).abs() < 1e-9);
    }

    #[test]
    fn unknown_legs_keep_mixed_totals_unknown_in_either_order() {
        let mut known = usage(100, 10);
        known.cost = Some(0.01);
        known.estimated = true;
        let unknown = usage(62_500, 6_000);
        assert_eq!(format_cost(&unknown), "spend unknown · 62.5k in / 6.0k out tokens");
        assert_eq!(format_cost(&Usage { total_tokens: 42, ..Default::default() }), "spend unknown · 42 tokens");
        for calls in [[known.clone(), unknown.clone()], [unknown.clone(), known.clone()]] {
            let mut total = Usage::default();
            for call in calls { total.add(&call); }
            assert_eq!(total.cost, None);
            assert!(format_cost(&total).contains("spend unknown"));
        }
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
