use std::io::IsTerminal;
use std::path::PathBuf;

use crate::engine::Effort;
use crate::error::SlagError;
use crate::sexp::Ingot;

/// File paths used by the pipeline
pub const BLUEPRINT: &str = "BLUEPRINT.md";
pub const CRUCIBLE: &str = "PLAN.md";
pub const ORE_FILE: &str = "PRD.md";
pub const ALLOY_FILE: &str = "AGENTS.md";
pub const LEDGER: &str = "PROGRESS.md";
pub const LOG_DIR: &str = "logs";

pub fn prepare_project() -> std::io::Result<()> {
    if std::path::Path::new(CRUCIBLE).exists() || std::path::Path::new(ORE_FILE).exists() {
        return Ok(());
    }
    match std::fs::symlink_metadata(".slag") {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "the .slag directory must be a regular workspace directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(".slag"),
        Err(e) => Err(e),
    }?;
    match std::fs::symlink_metadata(".slag/layout") {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "the .slag layout marker must be a regular file")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::write(".slag/layout", "1\n"),
        Err(e) => Err(e),
    }
}

pub fn project_path(name: &'static str) -> &'static str {
    project_relative(std::path::Path::new("."), name)
}

pub fn project_path_in(root: &std::path::Path, name: &'static str) -> PathBuf {
    root.join(project_relative(root, name))
}

fn project_relative(root: &std::path::Path, name: &'static str) -> &'static str {
    let marked = std::fs::symlink_metadata(root.join(".slag/layout"))
        .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink());
    if !marked && (root.join(CRUCIBLE).exists() || root.join(ORE_FILE).exists() || !root.join(".slag").is_dir()) {
        return name;
    }
    match name {
        BLUEPRINT => ".slag/BLUEPRINT.md",
        CRUCIBLE => ".slag/PLAN.md",
        ORE_FILE => ".slag/PRD.md",
        ALLOY_FILE => ".slag/AGENTS.md",
        LEDGER => ".slag/PROGRESS.md",
        "BAR.md" => ".slag/BAR.md",
        LOG_DIR => ".slag/logs",
        "logs/checkpoints" => ".slag/logs/checkpoints",
        "logs/transcripts" => ".slag/logs/transcripts",
        "logs/facets" => ".slag/logs/facets",
        "logs/bg" => ".slag/logs/bg",
        "logs/tool-results" => ".slag/logs/tool-results",
        _ => name,
    }
}


/// Behavior constants
pub const MAX_ANVILS: usize = 3;
pub const HIGH_GRADE: u8 = 3;
pub const MAX_ITERATE: usize = 3;

// --- Spend caps ---------------------------------------------------------
//
// Free functions rather than `EngineConfig` fields: the struct is built as
// a full literal in the duel/smith test constructors, so new fields would
// break files outside this change's blast radius. Default None = uncapped.

/// Dollar ceiling for a single ingot session (`SLAG_MAX_COST_INGOT` env,
/// `max_cost_per_ingot` file key). Enforced by the agent loop.
pub fn ingot_cost_cap() -> Option<f64> {
    cost_cap("SLAG_MAX_COST_INGOT", "max_cost_per_ingot")
}

/// Dollar ceiling for the whole run (`SLAG_MAX_COST_RUN` env,
/// `max_cost_per_run` file key). Enforced by the forge scheduler.
pub fn run_cost_cap() -> Option<f64> {
    cost_cap("SLAG_MAX_COST_RUN", "max_cost_per_run")
}

/// Account-balance floor (`SLAG_CREDIT_FLOOR` env, `credit_floor` file
/// key). Forge start warns when the OpenRouter balance sits under it, so a
/// long unattended run does not die mid-ingot on an empty account.
pub fn credit_floor() -> Option<f64> {
    cost_cap("SLAG_CREDIT_FLOOR", "credit_floor")
}

fn cost_cap(env_var: &str, file_key: &str) -> Option<f64> {
    env_nonempty(env_var)
        .or_else(|| {
            let entries = config_file_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|c| parse_config_lines(&c))
                .unwrap_or_default();
            file_value(&entries, file_key)
        })
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
}

/// Env-or-file string setting, same precedence as the cost caps: env wins,
/// then the config file, then None.
fn env_or_file(env_var: &str, file_key: &str) -> Option<String> {
    env_nonempty(env_var).or_else(|| {
        let entries = config_file_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|c| parse_config_lines(&c))
            .unwrap_or_default();
        file_value(&entries, file_key)
    })
}

/// Fallback model for capacity errors (`SLAG_MODEL_FALLBACK` env,
/// `fallback_model` file key). When set, the provider sends OpenRouter's
/// native `models: [primary, fallback]` routing array so a saturated
/// primary fails over inside one request — no extra round trip. Default
/// None = no fallback.
pub fn fallback_model() -> Option<String> {
    env_or_file("SLAG_MODEL_FALLBACK", "fallback_model")
}

/// The harness editor's model (`SLAG_MODEL_EDITOR` env, `model_editor`
/// file key), for `slag fix --evolve`. `None` means the plan model.
pub fn editor_model() -> Option<String> {
    env_or_file("SLAG_MODEL_EDITOR", "model_editor")
}

/// The harness library directory (`SLAG_HARNESS_LIBRARY` env,
/// `harness_library` file key), `~/` expanded. `None` lets the library
/// pick the repository's own or the per-user default.
pub fn harness_library() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    env_or_file("SLAG_HARNESS_LIBRARY", "harness_library")
        .and_then(|v| parse_path_list(&v, home.as_deref()).into_iter().next())
}

/// Dollar ceiling for everything one `slag fix --evolve` spends
/// (`SLAG_EVOLVE_BUDGET` env, `evolve_budget` file key).
pub fn evolve_budget() -> Option<f64> {
    cost_cap("SLAG_EVOLVE_BUDGET", "evolve_budget")
}

/// Unattended persistent-retry mode (`SLAG_UNATTENDED_RETRY` env,
/// `unattended_retry` file key). Truthy values: 1/true/on/yes. When on,
/// capacity errors (429/529) retry past the normal attempt budget with
/// backoff capped at 5 minutes — slag is an unattended orchestrator, and
/// an overnight run should outlast a rate-limit window, not crack on it.
pub fn unattended_retry() -> bool {
    parse_truthy(env_or_file("SLAG_UNATTENDED_RETRY", "unattended_retry"))
}

/// 1/true/on/yes (any case) are on; everything else, including unset, off.
fn parse_truthy(raw: Option<String>) -> bool {
    raw.map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "on" | "yes"))
        .unwrap_or(false)
}

/// Run-wide spend accumulator, shared by every anvil in the process.
/// Millicents (1/1000 of a cent) in a u64: f64 has no atomic add, and at
/// this resolution u64 never saturates on real spend.
static RUN_SPEND_MILLICENTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn dollars_to_millicents(dollars: f64) -> u64 {
    (dollars.max(0.0) * 100_000.0).round() as u64
}

fn millicents_to_dollars(millicents: u64) -> f64 {
    millicents as f64 / 100_000.0
}

/// Fold one response's cost into the run total (agent loop calls this).
pub fn add_run_spend(dollars: f64) {
    let mc = dollars_to_millicents(dollars);
    if mc > 0 {
        RUN_SPEND_MILLICENTS.fetch_add(mc, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Total dollars spent by this run so far.
pub fn run_spend_dollars() -> f64 {
    millicents_to_dollars(RUN_SPEND_MILLICENTS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Default model for every role: OpenRouter's automatic router picks a
/// live model per request, so a fresh key works with zero model config.
/// Each role still takes an env/file/flag override.
pub const AUTO_MODEL: &str = "openrouter/auto";

/// Chat-completions endpoint presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPreset {
    OpenRouter,
    OpenAI,
    Anthropic,
    Gemini,
    Groq,
    Cerebras,
    Xai,
    Mistral,
    DeepSeek,
    Together,
    Fireworks,
    Ollama,
    LmStudio,
    Custom,
}

impl ProviderPreset {
    pub fn parse(name: &str) -> Result<Self, SlagError> {
        match name.trim().to_ascii_lowercase().as_str() {
            "openrouter" => Ok(Self::OpenRouter),
            "openai" => Ok(Self::OpenAI),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            "groq" => Ok(Self::Groq),
            "cerebras" => Ok(Self::Cerebras),
            "xai" => Ok(Self::Xai),
            "mistral" => Ok(Self::Mistral),
            "deepseek" => Ok(Self::DeepSeek),
            "together" => Ok(Self::Together),
            "fireworks" => Ok(Self::Fireworks),
            "ollama" => Ok(Self::Ollama),
            "lmstudio" => Ok(Self::LmStudio),
            "custom" => Ok(Self::Custom),
            _ => Err(SlagError::Config(format!("unknown provider {name:?}"))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Groq => "groq",
            Self::Cerebras => "cerebras",
            Self::Xai => "xai",
            Self::Mistral => "mistral",
            Self::DeepSeek => "deepseek",
            Self::Together => "together",
            Self::Fireworks => "fireworks",
            Self::Ollama => "ollama",
            Self::LmStudio => "lmstudio",
            Self::Custom => "custom",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::OpenRouter => crate::engine::OPENROUTER_BASE,
            Self::OpenAI => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::Cerebras => "https://api.cerebras.ai/v1",
            Self::Xai => "https://api.x.ai/v1",
            Self::Mistral => "https://api.mistral.ai/v1",
            Self::DeepSeek => "https://api.deepseek.com",
            Self::Together => "https://api.together.ai/v1",
            Self::Fireworks => "https://api.fireworks.ai/inference/v1",
            Self::Ollama => "http://localhost:11434/v1",
            Self::LmStudio => "http://localhost:1234/v1",
            Self::Custom => "",
        }
    }

    pub fn has_models_endpoint(self) -> bool {
        !matches!(self, Self::Anthropic | Self::Custom)
    }

    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::OpenAI => Some("OPENAI_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::Groq => Some("GROQ_API_KEY"),
            Self::Cerebras => Some("CEREBRAS_API_KEY"),
            Self::Xai => Some("XAI_API_KEY"),
            Self::Mistral => Some("MISTRAL_API_KEY"),
            Self::DeepSeek => Some("DEEPSEEK_API_KEY"),
            Self::Together => Some("TOGETHER_API_KEY"),
            Self::Fireworks => Some("FIREWORKS_API_KEY"),
            Self::Ollama | Self::LmStudio => None,
            Self::Custom => Some("CUSTOM_API_KEY"),
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenRouter => AUTO_MODEL,
            Self::OpenAI => "gpt-4.1-mini",
            Self::Anthropic => "claude-sonnet-4-5",
            Self::Gemini => "gemini-flash-latest",
            Self::Groq => "openai/gpt-oss-120b",
            Self::Cerebras => "qwen-3.8-27b",
            Self::Xai => "grok-4.3",
            Self::Mistral => "mistral-small-latest",
            Self::DeepSeek => "deepseek-flash",
            Self::Together => "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            Self::Fireworks => "accounts/fireworks/models/glm-5p3-flash",
            Self::Ollama => "qwen3:8b",
            Self::LmStudio => "local-model",
            Self::Custom => "default",
        }
    }

    pub fn key_optional(self) -> bool {
        matches!(self, Self::Ollama | Self::LmStudio | Self::Custom)
    }
}

pub fn active_provider() -> Result<ProviderPreset, SlagError> {
    ProviderPreset::parse(&env_or_file("SLAG_PROVIDER", "provider")
        .unwrap_or_else(|| "openrouter".into()))
}

pub fn key_for_provider(provider: ProviderPreset) -> Option<String> {
    key_status_for_provider(provider).map(|(_, key)| key)
}

pub fn key_status_for_provider(provider: ProviderPreset) -> Option<(KeySource, String)> {
    if let Some(key) = provider.key_env().and_then(env_nonempty) {
        return Some((KeySource::Env, key));
    }
    if provider == ProviderPreset::Custom {
        if let Some(key) = env_nonempty("SLAG_API_KEY") {
            return Some((KeySource::Env, key));
        }
    }
    let entries = config_entries();
    file_value(&entries, &format!("{}_api_key", provider.name()))
        .map(|key| (KeySource::File, key))
}

pub fn provider_base_url(provider: ProviderPreset) -> String {
    let entries = config_entries();
    env_nonempty(&format!("SLAG_{}_BASE_URL", provider.name().to_ascii_uppercase()))
        .or_else(|| env_nonempty("SLAG_BASE_URL"))
        .or_else(|| (provider == ProviderPreset::OpenRouter)
            .then(|| env_nonempty("SLAG_OPENROUTER_BASE")).flatten())
        .or_else(|| file_value(&entries, &format!("{}_base_url", provider.name())))
        .or_else(|| file_value(&entries, "base_url"))
        .unwrap_or_else(|| provider.base_url().into())
        .trim_end_matches('/').to_string()
}

pub fn validate_provider_url(provider: ProviderPreset, base_url: &str) -> Result<(), SlagError> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|_| SlagError::Config("invalid provider base URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(SlagError::Config("provider base URL requires an HTTP(S) host".into()));
    }
    let host = url.host_str().unwrap().trim_end_matches('.');
    for (domain, owner) in [
        ("openrouter.ai", ProviderPreset::OpenRouter),
        ("openai.com", ProviderPreset::OpenAI),
        ("anthropic.com", ProviderPreset::Anthropic),
        ("generativelanguage.googleapis.com", ProviderPreset::Gemini),
        ("groq.com", ProviderPreset::Groq),
        ("cerebras.ai", ProviderPreset::Cerebras),
        ("x.ai", ProviderPreset::Xai),
        ("mistral.ai", ProviderPreset::Mistral),
        ("deepseek.com", ProviderPreset::DeepSeek),
        ("together.ai", ProviderPreset::Together),
        ("together.xyz", ProviderPreset::Together),
        ("fireworks.ai", ProviderPreset::Fireworks),
    ] {
        if provider != ProviderPreset::Custom && provider != owner
            && (host == domain || host.ends_with(&format!(".{domain}"))) {
            return Err(SlagError::Config(format!(
                "refusing to send {} credentials to the {} host; select --provider {}",
                provider.name(), owner.name(), owner.name()
            )));
        }
    }
    Ok(())
}

pub fn provider_for_model(model: &str) -> Result<Option<(ProviderPreset, String)>, SlagError> {
    let Some((name, model)) = model.split_once("::") else {
        return Ok(None);
    };
    let provider = ProviderPreset::parse(name)?;
    if model.trim().is_empty() {
        return Err(SlagError::Config(format!("no model after {}::", provider.name())));
    }
    Ok(Some((provider, model.to_string())))
}

pub fn resolve_connection(
    model: &str,
    fallback: ProviderPreset,
    key: &str,
    base_url: &str,
) -> Result<(ProviderPreset, String, String, String), SlagError> {
    let (provider, model) = provider_for_model(model)?
        .unwrap_or_else(|| (fallback, model.to_string()));
    let (key, base_url) = if provider == fallback {
        (key.to_string(), base_url.to_string())
    } else {
        (key_for_provider(provider).unwrap_or_default(), provider_base_url(provider))
    };
    if key.is_empty() && !provider.key_optional() {
        return Err(missing_key(provider));
    }
    if base_url.is_empty() {
        return Err(SlagError::Config(format!(
            "{} requires base_url or SLAG_BASE_URL", provider.name()
        )));
    }
    validate_provider_url(provider, &base_url)?;
    Ok((provider, key, base_url, model))
}

fn missing_key(provider: ProviderPreset) -> SlagError {
    SlagError::Config(format!(
        "no {} key. Set {}, or run `slag key --provider {}`",
        provider.name(), provider.key_env().unwrap_or("SLAG_API_KEY"), provider.name()
    ))
}

fn config_entries() -> Vec<(String, String)> {
    config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default()
}

fn role_model(entries: &[(String, String)], env: &str, file: &str, provider: ProviderPreset) -> String {
    let model = env_nonempty(env)
        .or_else(|| file_value(entries, file))
        .unwrap_or_else(|| provider.default_model().to_string());
    let model = if provider != ProviderPreset::OpenRouter && model == AUTO_MODEL {
        provider.default_model().to_string()
    } else {
        model
    };
    if provider != ProviderPreset::OpenRouter && !model.contains("::") {
        format!("{}::{model}", provider.name())
    } else {
        model
    }
}

/// Twin-cast duel gate. Auto duels only high grades; On/Off force it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuelMode {
    Auto,
    On,
    Off,
}

/// Native engine configuration resolved from environment + config file.
/// Env always overrides file; file lives at `~/.config/slag/config.toml`
/// (or `$SLAG_CONFIG_DIR/config.toml`).
#[derive(Clone)]
pub struct EngineConfig {
    pub api_key: String,
    pub model_base: String,
    pub model_plan: String,
    /// Cast B model. Set it to a different family than `model_base` to
    /// make a duel worth its cost; left unset it follows `model_base`.
    pub model_alt: String,
    /// Assayer model. Same story: a distinct family judges more usefully.
    pub model_judge: String,
    pub effort: Option<Effort>,
    pub base_url: String,
    pub duel: DuelMode,
    pub duel_rounds_override: Option<u8>,
    /// Shell command producing a screenshot for visual assay (web ingots).
    pub screenshot_cmd: Option<String>,
}

impl EngineConfig {
    /// Resolve the provider and validate the keys for the model roles.
    pub async fn resolve() -> Result<Self, SlagError> {
        let provider = active_provider()?;
        let mut cfg = match Self::load() {
            Some(cfg) => cfg,
            None => {
                onboard_provider(provider).await?;
                Self::load().ok_or_else(|| SlagError::Config("key stored but config still unreadable".into()))?
            }
        };
        let models = [&cfg.model_base, &cfg.model_plan, &cfg.model_alt, &cfg.model_judge];
        let mut uses_active = false;
        for model in models {
            let role_provider = provider_for_model(model)?.map(|(provider, _)| provider).unwrap_or(provider);
            uses_active |= role_provider == provider;
        }
        if cfg.api_key.is_empty() && !provider.key_optional() && uses_active {
            onboard_provider(provider).await?;
            cfg = Self::load().ok_or_else(|| SlagError::Config("key stored but config still unreadable".into()))?;
        }
        for model in [&cfg.model_base, &cfg.model_plan, &cfg.model_alt, &cfg.model_judge] {
            cfg.connection_for_model(model)?;
        }
        Ok(cfg)
    }

    /// Resolve config without a prompt. Preserve OpenRouter's absent-key result.
    pub fn load() -> Option<Self> {
        let entries = config_entries();
        let provider = active_provider().ok()?;
        let api_key = key_for_provider(provider).unwrap_or_default();

        let model_base = role_model(&entries, "SLAG_MODEL_BASE", "model_base", provider);
        let model_plan = role_model(&entries, "SLAG_MODEL_PLAN", "model_plan", provider);
        // Unset, cast B follows the resolved base model, so naming a base
        // model never opens a duel of that model against `openrouter/auto`.
        let model_alt = if env_nonempty("SLAG_MODEL_ALT").is_some()
            || file_value(&entries, "model_alt").is_some()
        {
            role_model(&entries, "SLAG_MODEL_ALT", "model_alt", provider)
        } else {
            model_base.clone()
        };
        let model_judge = role_model(&entries, "SLAG_MODEL_JUDGE", "model_judge", provider);

        let uses_active = [&model_base, &model_plan, &model_alt, &model_judge]
            .iter()
            .any(|model| match provider_for_model(model) {
                Ok(Some((role_provider, _))) => role_provider == provider,
                _ => true,
            });
        if api_key.is_empty() && provider == ProviderPreset::OpenRouter && uses_active {
            return None;
        }

        let duel = env_nonempty("SLAG_DUEL")
            .or_else(|| file_value(&entries, "duel"))
            .map(|v| match v.to_lowercase().as_str() {
                "on" => DuelMode::On,
                "off" => DuelMode::Off,
                _ => DuelMode::Auto,
            })
            .unwrap_or(DuelMode::Auto);

        let duel_rounds_override = env_nonempty("SLAG_DUEL_ROUNDS")
            .or_else(|| file_value(&entries, "duel_rounds"))
            .and_then(|v| v.parse::<u8>().ok());

        let screenshot_cmd =
            env_nonempty("SLAG_SCREENSHOT_CMD").or_else(|| file_value(&entries, "screenshot_cmd"));



        let effort =
            env_nonempty("SLAG_REASONING_EFFORT").and_then(|v| match v.to_lowercase().as_str() {
                "low" => Some(Effort::Low),
                "medium" => Some(Effort::Medium),
                "high" => Some(Effort::High),
                _ => None,
            });

        let base_url = provider_base_url(provider);

        Some(Self {
            api_key,
            model_base,
            model_plan,
            model_alt,
            model_judge,
            effort,
            base_url,
            duel,
            duel_rounds_override,
            screenshot_cmd,
        })
    }

    pub fn connection_for_model(
        &self,
        model: &str,
    ) -> Result<(ProviderPreset, String, String, String), SlagError> {
        resolve_connection(model, active_provider()?, &self.api_key, &self.base_url)
    }

    /// Select model by ingot grade: high grades get the plan/reasoning model.
    pub fn model_for_grade(&self, grade: u8) -> &str {
        if grade >= HIGH_GRADE {
            &self.model_plan
        } else {
            &self.model_base
        }
    }

    /// Grade-gated duel qualification (plan section 9 rule 1): Auto duels
    /// grade 3 and above, and only when `model_alt` differs from
    /// `model_base`. A duel of one model against itself cost about 60% more
    /// in measured builds and changed no outcome. `On` still duels
    /// matched models.
    pub fn duel_qualifies(&self, grade: u8) -> bool {
        match self.duel {
            DuelMode::On => true,
            DuelMode::Off => false,
            DuelMode::Auto => grade >= HIGH_GRADE && self.model_alt != self.model_base,
        }
    }

    /// Resolve how many casts forge this ingot (adaptive cast count).
    ///
    /// Sequential ingots always get one cast — overlapping sequential
    /// work from multiple casts is a merge-conflict factory. `SLAG_DUEL=off`
    /// is the user's kill switch and comes next: it must beat the pins
    /// below, which a user may have left in a plan. Then
    /// an explicit `:casts` pin wins, then the legacy `:duel` override,
    /// then `SLAG_DUEL=on` (at least 2), then the work-shape heuristics.
    /// The heuristics apply only when `model_alt` differs from
    /// `model_base`; with matched models Auto gives one cast.
    /// Crack-retry escalation (heat > 0 bumps 1 → 2) lives in the forge
    /// scheduler, not here.
    pub fn casts_for(&self, ingot: &Ingot) -> u8 {
        if !ingot.solo || self.duel == DuelMode::Off {
            return 1;
        }
        if let Some(n) = ingot.casts {
            return n.clamp(1, 3);
        }
        match ingot.duel {
            Some(false) => return 1,
            Some(true) => return heuristic_casts(ingot).max(2),
            None => {}
        }
        match self.duel {
            DuelMode::Off => 1,
            DuelMode::On => heuristic_casts(ingot).max(2),
            DuelMode::Auto if self.model_alt == self.model_base => 1,
            DuelMode::Auto => heuristic_casts(ingot),
        }
    }

    /// Duel round cap (plan section 9 rule 1): override wins, else max 3
    /// rounds at grade 3-4, studio mode up to 10 at grade 5 and above.
    pub fn duel_rounds(&self, grade: u8) -> u8 {
        if let Some(rounds) = self.duel_rounds_override {
            return rounds;
        }
        if grade >= 5 {
            10
        } else {
            3
        }
    }
}

/// Work-shape heuristics for the auto cast count: 3 casts for grade 5 or
/// taste-dominant polish work (a judge earns its keep where taste rules),
/// 1 cast for low grades, mechanical work, or a deterministic
/// file/pattern proof (nothing to arbitrate), else 2 for the
/// design-choice middle (grade 3-4).
fn heuristic_casts(ingot: &Ingot) -> u8 {
    if ingot.grade >= 5 || is_taste_dominant(&ingot.work) {
        return 3;
    }
    if ingot.grade < HIGH_GRADE
        || is_mechanical(&ingot.work)
        || is_deterministic_proof(&ingot.proof)
    {
        return 1;
    }
    2
}

/// Mechanical work has one right answer; a second cast buys nothing.
fn is_mechanical(work: &str) -> bool {
    let work = work.to_lowercase();
    ["create ", "rename", "config", "scaffold", "install ", "mkdir", "copy "]
        .iter()
        .any(|kw| work.contains(kw))
}

/// File-existence and pattern proofs verify a deterministic outcome:
/// every passing cast produced the same thing the proof checks for.
fn is_deterministic_proof(proof: &str) -> bool {
    let proof = proof.trim();
    proof.starts_with("test -") || proof.starts_with("grep -q")
}

/// Taste-dominant work has no single right answer; three directions
/// (minimal / robust / creative) give the judge a real spread to rank.
fn is_taste_dominant(work: &str) -> bool {
    let work = work.to_lowercase();
    ["polish", "aesthetic", "taste", "visual design", "look and feel", "beautiful"]
        .iter()
        .any(|kw| work.contains(kw))
}

/// Persist the OpenRouter key to the config file (0o600 on unix).
/// Preserves any other keys already present in the file.
pub fn store_key(key: &str) -> Result<PathBuf, SlagError> {
    store_key_for_provider(ProviderPreset::OpenRouter, key)
}

pub fn store_key_for_provider(provider: ProviderPreset, key: &str) -> Result<PathBuf, SlagError> {
    let dir = config_dir()
        .ok_or_else(|| SlagError::Config("cannot resolve config dir: $HOME not set".into()))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");

    let mut entries = std::fs::read_to_string(&path)
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default();
    let file_key = format!("{}_api_key", provider.name());
    match entries.iter_mut().find(|(k, _)| k == &file_key) {
        Some(entry) => entry.1 = key.to_string(),
        None => entries.push((file_key, key.to_string())),
    }

    write_entries(&path, &entries)?;
    Ok(path)
}

/// Rewrite the config file from `key = "value"` pairs, owner-only on unix.
fn write_entries(path: &std::path::Path, entries: &[(String, String)]) -> Result<(), SlagError> {
    let mut out = String::new();
    for (k, v) in entries {
        out.push_str(k);
        if k.starts_with("pricing.") && v.trim_start().starts_with('{') {
            out.push_str(" = ");
            out.push_str(v);
            out.push('\n');
            continue;
        }
        out.push_str(" = \"");
        out.push_str(v);
        out.push_str("\"\n");
    }
    std::fs::write(path, out)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

/// Forget the stored key. Leaves every other config entry in place.
#[cfg(test)]
pub fn clear_key() -> Result<Option<PathBuf>, SlagError> {
    let Some(path) = config_file_path() else {
        return Ok(None);
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let entries: Vec<_> = parse_config_lines(&contents)
        .into_iter()
        .filter(|(k, _)| k != "openrouter_api_key")
        .collect();
    write_entries(&path, &entries)?;
    Ok(Some(path))
}

/// Where the active key comes from. Env wins so CI can override a stored key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Env,
    File,
}

impl KeySource {
    #[cfg(test)]
    pub fn label(self) -> &'static str {
        match self {
            KeySource::Env => "OPENROUTER_API_KEY",
            KeySource::File => "config file",
        }
    }
}

/// Current key and where it came from, without onboarding.
#[cfg(test)]
pub fn key_status() -> Option<(KeySource, String)> {
    if let Some(key) = env_nonempty("OPENROUTER_API_KEY") {
        return Some((KeySource::Env, key));
    }
    let entries = config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default();
    file_value(&entries, "openrouter_api_key").map(|key| (KeySource::File, key))
}

/// Show a key without leaking it: first 7 and last 4 characters.
#[cfg(test)]
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 14 {
        return "•".repeat(chars.len().max(1));
    }
    let head: String = chars[..7].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// The base URL a key is checked against; honours the test/proxy override.
pub fn base_url() -> String {
    provider_base_url(ProviderPreset::OpenRouter)
}

/// Interactive key onboarding: read, verify against OpenRouter, then store.
/// A key that never gets verified fails later inside a forge, where the
/// error reads like a model problem instead of a setup problem.
/// Headless runs get an actionable error rather than a blocked stdin read.
pub async fn onboard() -> Result<String, SlagError> {
    if !std::io::stdin().is_terminal() {
        return Err(SlagError::Config(
            "no OpenRouter key. Set OPENROUTER_API_KEY, or run `slag key` on a terminal \
             to save one. Get a key at https://openrouter.ai/workspaces/default/keys"
                .into(),
        ));
    }

    crate::tui::key_intro();
    let key = read_key_line()?;
    verify_and_store(&key).await?;
    Ok(key)
}

/// Verify a key over the wire, then persist it. Shared by onboarding and
/// `slag key`, so neither saves a key OpenRouter refuses.
///
/// Only a refusal blocks. A key typed on a plane is still probably the
/// right key: slag saves it with a warning rather than making the user
/// find it again once the network comes back.
pub async fn verify_and_store(key: &str) -> Result<PathBuf, SlagError> {
    use crate::engine::provider::KeyCheck;

    if key.is_empty() {
        return Err(SlagError::Config("no key entered".into()));
    }

    let spinner = crate::tui::spinner("checking key with OpenRouter");
    let check = crate::engine::provider::check_key(key, &base_url()).await;
    spinner.finish_and_clear();

    match check {
        KeyCheck::Valid => {}
        KeyCheck::Rejected(why) => {
            return Err(SlagError::Config(format!(
                "OpenRouter rejected that key ({why}). \
                 Copy it again from https://openrouter.ai/workspaces/default/keys"
            )))
        }
        KeyCheck::Unreachable(why) => crate::tui::key_unverified(&why),
    }

    let path = store_key(key)?;
    crate::tui::key_saved(&path);
    Ok(path)
}

pub async fn verify_and_store_for_provider(
    provider: ProviderPreset,
    key: &str,
) -> Result<PathBuf, SlagError> {
    if provider == ProviderPreset::OpenRouter {
        return verify_and_store(key).await;
    }
    if key.trim().is_empty() {
        return Err(SlagError::Config("no key entered".into()));
    }
    let path = store_key_for_provider(provider, key)?;
    crate::tui::key_saved(&path);
    Ok(path)
}

pub async fn onboard_provider(provider: ProviderPreset) -> Result<String, SlagError> {
    if provider == ProviderPreset::OpenRouter {
        return onboard().await;
    }
    if !std::io::stdin().is_terminal() {
        return Err(missing_key(provider));
    }
    println!("  Enter the {} API key:", provider.name());
    let key = read_key_line()?;
    verify_and_store_for_provider(provider, &key).await?;
    Ok(key)
}

/// Read one key from stdin, rejecting blanks before any network call.
fn read_key_line() -> Result<String, SlagError> {
    use std::io::BufRead;

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err(SlagError::Config("no key entered".into()));
    }
    Ok(key)
}

/// Serializes every test that mutates process env. Env is per-process and
/// tests run in parallel, so without this one test's `SLAG_CONFIG_DIR`
/// leaks into another's.
///
/// It also guards a subtler failure: a test that leaves the variable unset
/// reads the developer's real `~/.config/slag`. `tolerates_null_content_
/// and_missing_usage` did exactly that -- it passed on a machine that had
/// never run slag and failed on one that had, because the pricing cache
/// written by a real forge gave a costless response a cost.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A config dir of its own, for a test that must not read the real one.
/// Returns the guard and the tempdir; both must stay alive for the test.
#[cfg(test)]
pub(crate) fn isolated_config_dir(
) -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("SLAG_CONFIG_DIR", dir.path());
    (guard, dir)
}

/// Config directory: $SLAG_CONFIG_DIR override (tests), else ~/.config/slag.
fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SLAG_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("slag"))
}

/// Public view of the config directory, for sibling caches that live
/// beside `config.toml` (the pricing table, item 34).
pub fn config_dir_path() -> Option<PathBuf> {
    config_dir()
}

/// File prices in USD per million input, output, and cached input tokens.
pub fn pricing_overrides() -> std::collections::HashMap<String, (f64, f64, Option<f64>)> {
    config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|contents| parse_pricing_overrides(&contents))
        .unwrap_or_default()
}

fn parse_pricing_overrides(
    contents: &str,
) -> std::collections::HashMap<String, (f64, f64, Option<f64>)> {
    let mut prices = std::collections::HashMap::new();
    let mut in_pricing = false;
    let mut top_level = true;
    for line in contents.lines() {
        let line = line[..unquoted_position(line, '#').unwrap_or(line.len())].trim();
        if line.starts_with('[') {
            in_pricing = line == "[pricing]";
            top_level = false;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let Some(separator) = unquoted_position(line, '=') else {
            continue;
        };
        let raw_key = line[..separator].trim();
        let raw_key = if in_pricing {
            raw_key
        } else if top_level {
            let Some(key) = raw_key.strip_prefix("pricing.") else { continue };
            key
        } else {
            continue;
        };
        let key = if raw_key.starts_with('"') {
            serde_json::from_str::<String>(raw_key).ok()
        } else {
            raw_key.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).map(str::to_string)
        };
        let Some(key) = key else { continue };
        let Some((provider, model)) = key.split_once("::") else { continue };
        if ProviderPreset::parse(provider).is_err() || model.trim().is_empty() {
            continue;
        }
        let Some(body) = line[separator + 1..].trim().strip_prefix('{')
            .and_then(|s| s.strip_suffix('}')) else { continue };
        let mut input = None;
        let mut output = None;
        let mut cached_input = None;
        let mut valid = true;
        for field in body.split(',') {
            let Some((name, raw_value)) = field.split_once('=') else {
                valid = false;
                break;
            };
            let target = match name.trim() {
                "input" => &mut input,
                "output" => &mut output,
                "cached_input" => &mut cached_input,
                _ => { valid = false; break; }
            };
            let Some(value) = raw_value.trim().parse::<f64>().ok()
                .filter(|value| value.is_finite() && *value >= 0.0) else {
                valid = false;
                break;
            };
            if target.replace(value).is_some() {
                valid = false;
                break;
            }
        }
        if valid {
            if let (Some(input), Some(output)) = (input, output) {
                prices.insert(key, (input, output, cached_input));
            }
        }
    }
    prices
}

fn unquoted_position(text: &str, needle: char) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if quote == Some('"') && ch == '\\' {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() {
            if ch == needle {
                return Some(index);
            }
            if ch == '"' || ch == '\'' {
                quote = Some(ch);
            }
        }
    }
    None
}

fn config_file_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Read an env var, treating empty/whitespace values as unset.
fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Hand-parse simple `key = "value"` lines. No toml dependency.
/// Skips blanks, comments, and lines without `=`. Quotes optional.
fn parse_config_lines(contents: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut section = String::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `[section]` headers namespace the keys under them, so the flat
        // top-level settings and an `[mcp]` server named `model_base`
        // cannot collide.
        let header = line[..unquoted_position(line, '#').unwrap_or(line.len())].trim();
        if let Some(name) = header.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some(separator) = unquoted_position(line, '=') else {
            continue;
        };
        let (key, value) = (&line[..separator], &line[separator + 1..]);
        let key = key.trim();
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        if !key.is_empty() && !value.is_empty() {
            let key = if section.is_empty() {
                key.to_string()
            } else {
                format!("{section}.{key}")
            };
            entries.push((key, value.to_string()));
        }
    }
    entries
}

/// MCP stdio servers from the `[mcp]` table: one `name = "command args…"`
/// line each. File-only, no env override — a server is a local command,
/// not a per-run knob.
pub fn mcp_servers() -> Vec<(String, String)> {
    let entries = config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|(key, value)| key.strip_prefix("mcp.").map(|n| (n.to_string(), value)))
        .collect()
}

/// Policy rules from the `[policy]` table: `deny` / `ask` / `allow`
/// lines, values passed through raw for `engine::policy` to parse.
/// File-only, no env override, same posture as `mcp_servers`.
pub fn policy_entries() -> Vec<(String, String)> {
    let entries = config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|(key, value)| key.strip_prefix("policy.").map(|n| (n.to_string(), value)))
        .collect()
}

/// Lifecycle hooks from the `[hooks]` table: one `event = "field=value…"`
/// line each, the event name as key. Keys repeat freely — several hooks
/// on one event is the normal case — so this returns pairs, not a map,
/// and `engine::hooks` parses the values.
///
/// File-only, no env override: a hook is a local side-effect the operator
/// wired up, not a per-run knob.
pub fn hook_entries() -> Vec<(String, String)> {
    let entries = config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|(key, value)| key.strip_prefix("hooks.").map(|n| (n.to_string(), value)))
        .collect()
}

/// Kill switch: `disable_all_hooks = "t"` in the config file, or
/// `SLAG_DISABLE_HOOKS` in the environment. The env spelling wins so an
/// operator can silence a misbehaving hook for one run without editing
/// the file a smith may be about to rewrite.
pub fn disable_all_hooks() -> bool {
    if let Some(v) = env_nonempty("SLAG_DISABLE_HOOKS") {
        return truthy(&v);
    }
    config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default()
        .iter()
        .find(|(k, _)| k == "disable_all_hooks")
        .is_some_and(|(_, v)| truthy(v))
}

/// Shell sandbox switch (`engine::sandbox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Confine shell writes with the platform backend when one works.
    Auto,
    /// Run shell commands unconfined.
    Off,
}

/// The shell sandbox setting and its extra writable paths. Mode resolves
/// `--no-sandbox` (main sets `SLAG_SANDBOX=off`), then `SLAG_SANDBOX`,
/// then `sandbox` in the file. Extras come from the file only:
/// `sandbox_writable = ["~/data", "/srv/scratch"]`, `~/` expanded.
pub fn sandbox_settings() -> (SandboxMode, Vec<PathBuf>) {
    let entries = config_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default();
    let mode = sandbox_mode_from(env_nonempty("SLAG_SANDBOX"), file_value(&entries, "sandbox"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let extras = file_value(&entries, "sandbox_writable")
        .map(|v| parse_path_list(&v, home.as_deref()))
        .unwrap_or_default();
    (mode, extras)
}

/// Env beats file. Only an explicit off value disables the sandbox, so a
/// typo keeps it on.
fn sandbox_mode_from(env: Option<String>, file: Option<String>) -> SandboxMode {
    let value = env.or(file).unwrap_or_default().trim().to_ascii_lowercase();
    match value.as_str() {
        "off" | "false" | "0" | "no" | "nil" => SandboxMode::Off,
        _ => SandboxMode::Auto,
    }
}

/// `["a", "b"]` or `a, b`, quotes optional, a leading `~/` expanded.
fn parse_path_list(value: &str, home: Option<&std::path::Path>) -> Vec<PathBuf> {
    let value = value.trim();
    let inner = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')).unwrap_or(value);
    inner
        .split(',')
        .map(|item| item.trim().trim_matches(|c| c == '"' || c == '\''))
        .filter(|item| !item.is_empty())
        .filter_map(|item| match item.strip_prefix("~/") {
            Some(rest) => home.map(|h| h.join(rest)),
            None => Some(PathBuf::from(item)),
        })
        .collect()
}

/// slag spells booleans `t`/`nil` in ingots; config files reach for
/// `true`/`1`/`yes`. Accept all of them, and treat `nil`/`false`/`0`/`off`
/// as the explicit no.
pub fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "t" | "true" | "1" | "yes" | "on"
    )
}

fn file_value(entries: &[(String, String)], key: &str) -> Option<String> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Env vars are process-global; serialize every test that touches them.
    use crate::config::ENV_LOCK;

    const ENGINE_VARS: &[&str] = &[
        "OPENROUTER_API_KEY",
        "SLAG_MODEL_BASE",
        "SLAG_MODEL_PLAN",
        "SLAG_MODEL_ALT",
        "SLAG_MODEL_JUDGE",
        "SLAG_DUEL",
        "SLAG_DUEL_ROUNDS",
        "SLAG_SCREENSHOT_CMD",
        "SLAG_REASONING_EFFORT",
        "SLAG_OPENROUTER_BASE",
        "SLAG_CONFIG_DIR",
        "SLAG_MAX_COST_INGOT",
        "SLAG_MAX_COST_RUN",
        "SLAG_MODEL_FALLBACK",
        "SLAG_UNATTENDED_RETRY",
        "SLAG_PROVIDER",
        "SLAG_BASE_URL",
        "SLAG_API_KEY",
        "CUSTOM_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "GROQ_API_KEY",
        "CEREBRAS_API_KEY",
        "XAI_API_KEY",
        "MISTRAL_API_KEY",
        "DEEPSEEK_API_KEY",
        "TOGETHER_API_KEY",
        "FIREWORKS_API_KEY",
        "SLAG_CEREBRAS_BASE_URL",
    ];

    fn clear_engine_env() {
        for var in ENGINE_VARS {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn provider_presets_define_endpoints_keys_and_defaults() {
        for (name, key, model) in [
            ("openrouter", Some("OPENROUTER_API_KEY"), AUTO_MODEL),
            ("openai", Some("OPENAI_API_KEY"), "gpt-4.1-mini"),
            ("anthropic", Some("ANTHROPIC_API_KEY"), "claude-sonnet-4-5"),
            ("gemini", Some("GEMINI_API_KEY"), "gemini-flash-latest"),
            ("groq", Some("GROQ_API_KEY"), "openai/gpt-oss-120b"),
            ("cerebras", Some("CEREBRAS_API_KEY"), "qwen-3.8-27b"),
            ("xai", Some("XAI_API_KEY"), "grok-4.3"),
            ("mistral", Some("MISTRAL_API_KEY"), "mistral-small-latest"),
            ("deepseek", Some("DEEPSEEK_API_KEY"), "deepseek-flash"),
            ("together", Some("TOGETHER_API_KEY"), "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
            ("fireworks", Some("FIREWORKS_API_KEY"), "accounts/fireworks/models/glm-5p3-flash"),
            ("ollama", None, "qwen3:8b"),
            ("lmstudio", None, "local-model"),
            ("custom", Some("CUSTOM_API_KEY"), "default"),
        ] {
            let preset = ProviderPreset::parse(name).unwrap();
            assert_eq!(preset.name(), name);
            assert_eq!(preset.key_env(), key);
            assert_eq!(preset.default_model(), model);
            assert_eq!(preset.key_optional(), matches!(name, "ollama" | "lmstudio" | "custom"));
            if name != "custom" {
                assert!(preset.base_url().starts_with("http"));
            }
        }
        assert!(ProviderPreset::parse("unknown").is_err());
        assert_eq!(ProviderPreset::parse(" OpenAI ").unwrap(), ProviderPreset::OpenAI);
    }

    #[test]
    fn provider_env_overrides_file_and_models_use_qualified_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(dir.path().join("config.toml"),
            "provider = openai\nopenai_api_key = file-openai\ncerebras_api_key = file-cerebras\nbase_url = http://file/v1\n").unwrap();
        let cfg = EngineConfig::load().unwrap();
        assert_eq!(active_provider().unwrap(), ProviderPreset::OpenAI);
        assert_eq!(cfg.model_base, "openai::gpt-4.1-mini");
        assert_eq!(cfg.base_url, "http://file/v1");
        std::env::set_var("SLAG_PROVIDER", "cerebras");
        std::env::set_var("CEREBRAS_API_KEY", "env-cerebras");
        std::env::set_var("SLAG_BASE_URL", "http://env/v1/");
        std::env::set_var("SLAG_MODEL_PLAN", "openai::gpt-4.1-mini");
        let cfg = EngineConfig::load().unwrap();
        assert_eq!(cfg.api_key, "env-cerebras");
        assert_eq!(cfg.model_base, "cerebras::qwen-3.8-27b");
        assert_eq!(cfg.model_plan, "openai::gpt-4.1-mini");
        assert_eq!(cfg.model_alt, cfg.model_base);
        assert_eq!(cfg.model_judge, cfg.model_base);
        assert_eq!(cfg.base_url, "http://env/v1");
        clear_engine_env();
    }

    #[test]
    fn legacy_endpoint_alias_only_applies_to_openrouter() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("SLAG_OPENROUTER_BASE", "http://legacy/v1");
        assert_eq!(provider_base_url(ProviderPreset::OpenRouter), "http://legacy/v1");
        assert_eq!(provider_base_url(ProviderPreset::Cerebras), ProviderPreset::Cerebras.base_url());
        std::env::set_var("SLAG_BASE_URL", "http://global/v1");
        assert_eq!(provider_base_url(ProviderPreset::Cerebras), "http://global/v1");
        std::env::set_var("SLAG_CEREBRAS_BASE_URL", "http://cerebras/v1");
        assert_eq!(provider_base_url(ProviderPreset::Cerebras), "http://cerebras/v1");
        clear_engine_env();
    }

    #[test]
    fn provider_credentials_reject_other_preset_hosts() {
        let providers = [
            ProviderPreset::OpenRouter, ProviderPreset::OpenAI, ProviderPreset::Anthropic,
            ProviderPreset::Gemini, ProviderPreset::Groq, ProviderPreset::Cerebras,
            ProviderPreset::Xai, ProviderPreset::Mistral, ProviderPreset::DeepSeek,
            ProviderPreset::Together, ProviderPreset::Fireworks,
        ];
        for provider in providers {
            for destination in providers {
                let connection = resolve_connection("model", provider, "test-key", destination.base_url());
                assert_eq!(connection.is_ok(), provider == destination,
                    "{} key to {} host", provider.name(), destination.name());
            }
            assert!(validate_provider_url(provider, "http://127.0.0.1:1234/v1").is_ok());
            assert!(validate_provider_url(provider, "https://proxy.example/v1").is_ok());
        }
        assert!(validate_provider_url(ProviderPreset::Cerebras, "https://OPENROUTER.AI./api/v1").is_err());
        assert!(validate_provider_url(ProviderPreset::Cerebras, "https://proxy.openrouter.ai/v1").is_err());
        assert!(validate_provider_url(ProviderPreset::OpenAI, "https://api.together.xyz/v1").is_err());
        assert!(validate_provider_url(ProviderPreset::OpenAI, "file:///tmp/socket").is_err());
        assert!(validate_provider_url(ProviderPreset::Custom, ProviderPreset::OpenAI.base_url()).is_ok());
    }

    #[test]
    fn endpoint_overrides_cannot_route_preset_keys_to_another_preset() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("SLAG_PROVIDER", "cerebras");
        std::env::set_var("CEREBRAS_API_KEY", "cerebras-key");
        std::env::set_var("OPENAI_API_KEY", "openai-key");
        std::env::set_var("SLAG_OPENROUTER_BASE", ProviderPreset::OpenRouter.base_url());
        let cfg = EngineConfig::load().unwrap();
        assert_eq!(cfg.base_url, ProviderPreset::Cerebras.base_url());
        assert!(cfg.connection_for_model(&cfg.model_base).is_ok());
        assert_eq!(cfg.connection_for_model("openai::gpt-4.1-mini").unwrap().2,
            ProviderPreset::OpenAI.base_url());
        for variable in ["SLAG_BASE_URL", "SLAG_CEREBRAS_BASE_URL"] {
            std::env::set_var(variable, ProviderPreset::OpenRouter.base_url());
            let cfg = EngineConfig::load().unwrap();
            assert!(cfg.connection_for_model(&cfg.model_base).is_err());
            std::env::remove_var(variable);
        }
        for setting in ["base_url", "cerebras_base_url"] {
            std::fs::write(dir.path().join("config.toml"), format!(
                "{setting} = \"{}\"\n", ProviderPreset::OpenRouter.base_url())).unwrap();
            let cfg = EngineConfig::load().unwrap();
            assert!(cfg.connection_for_model(&cfg.model_base).is_err());
        }
        std::env::set_var("SLAG_BASE_URL", ProviderPreset::Cerebras.base_url());
        assert!(EngineConfig::load().unwrap().connection_for_model("openai::gpt-4.1-mini").is_err());
        clear_engine_env();
    }

    #[test]
    fn mixed_role_connections_keep_openrouter_ids_and_select_named_keys() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("OPENROUTER_API_KEY", "router-key");
        std::env::set_var("OPENAI_API_KEY", "openai-key");
        std::env::set_var("CEREBRAS_API_KEY", "cerebras-key");
        std::env::set_var("SLAG_MODEL_BASE", "cerebras::gpt-oss-120b");
        std::env::set_var("SLAG_MODEL_PLAN", "openai::gpt-4.1-mini");
        let cfg = EngineConfig::load().unwrap();
        let (provider, key, base, model) = cfg.connection_for_model(&cfg.model_base).unwrap();
        assert_eq!((provider, key.as_str(), base.as_str(), model.as_str()),
            (ProviderPreset::Cerebras, "cerebras-key", ProviderPreset::Cerebras.base_url(), "gpt-oss-120b"));
        let (provider, key, _, model) = cfg.connection_for_model(&cfg.model_plan).unwrap();
        assert_eq!((provider, key.as_str(), model.as_str()),
            (ProviderPreset::OpenAI, "openai-key", "gpt-4.1-mini"));
        let (provider, key, _, model) = cfg.connection_for_model("openai/gpt-5").unwrap();
        assert_eq!((provider, key.as_str(), model.as_str()),
            (ProviderPreset::OpenRouter, "router-key", "openai/gpt-5"));
        assert!(cfg.connection_for_model("unknown::model").is_err());
        assert!(cfg.connection_for_model("openai::").is_err());
        assert!(cfg.connection_for_model("groq::model").unwrap_err().to_string().contains("GROQ_API_KEY"));
        clear_engine_env();
    }

    #[tokio::test]
    async fn missing_non_openrouter_key_keeps_config_readable_and_fails_before_network() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("SLAG_PROVIDER", "cerebras");
        std::env::set_var("SLAG_MODEL_PLAN", "openai::gpt-4.1-mini");
        let server = wiremock::MockServer::start().await;
        std::env::set_var("SLAG_BASE_URL", server.uri());
        let cfg = EngineConfig::load().expect("key status and cost can read config without a key");
        assert!(cfg.api_key.is_empty());
        assert_eq!(cfg.model_plan, "openai::gpt-4.1-mini");
        assert!(cfg.connection_for_model(&cfg.model_base).unwrap_err().to_string().contains("CEREBRAS_API_KEY"));
        if !std::io::stdin().is_terminal() {
            let error = EngineConfig::resolve().await.err().expect("forge requires a key");
            assert!(error.to_string().contains("CEREBRAS_API_KEY"));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
        clear_engine_env();
    }

    #[test]
    fn local_and_custom_configs_accept_no_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        for name in ["ollama", "lmstudio"] {
            std::env::set_var("SLAG_PROVIDER", name);
            let cfg = EngineConfig::load().unwrap();
            assert!(cfg.api_key.is_empty());
            assert!(cfg.connection_for_model(&cfg.model_base).is_ok());
        }
        std::env::set_var("SLAG_PROVIDER", "custom");
        let cfg = EngineConfig::load().unwrap();
        assert!(cfg.connection_for_model(&cfg.model_base).unwrap_err().to_string().contains("base_url"));
        std::env::set_var("SLAG_BASE_URL", "http://custom/v1");
        let cfg = EngineConfig::load().unwrap();
        assert!(cfg.connection_for_model(&cfg.model_base).is_ok());
        std::env::set_var("SLAG_API_KEY", "custom-key");
        assert_eq!(key_for_provider(ProviderPreset::Custom).as_deref(), Some("custom-key"));
        clear_engine_env();
    }

    #[test]
    fn all_qualified_roles_do_not_need_an_unused_active_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        for var in ["SLAG_MODEL_BASE", "SLAG_MODEL_PLAN", "SLAG_MODEL_ALT", "SLAG_MODEL_JUDGE"] {
            std::env::set_var(var, "ollama::qwen3:8b");
        }
        let cfg = EngineConfig::load().unwrap();
        assert!(cfg.api_key.is_empty());
        assert!(cfg.connection_for_model(&cfg.model_base).is_ok());
        clear_engine_env();
    }

    #[test]
    fn non_openrouter_auto_uses_its_preset_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("SLAG_PROVIDER", "ollama");
        std::env::set_var("SLAG_MODEL_BASE", AUTO_MODEL);
        std::env::set_var("SLAG_MODEL_PLAN", "openrouter::openrouter/auto");
        let cfg = EngineConfig::load().unwrap();
        assert_eq!(cfg.model_base, "ollama::qwen3:8b");
        assert_eq!(cfg.model_plan, "openrouter::openrouter/auto");
        clear_engine_env();
    }

    #[tokio::test]
    async fn named_key_storage_never_calls_the_openrouter_key_endpoint() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        let server = wiremock::MockServer::start().await;
        std::env::set_var("SLAG_OPENROUTER_BASE", server.uri());
        verify_and_store_for_provider(ProviderPreset::Cerebras, "named-key").await.unwrap();
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(key_for_provider(ProviderPreset::Cerebras).as_deref(), Some("named-key"));
        assert!(key_for_provider(ProviderPreset::OpenRouter).is_none());
        clear_engine_env();
    }

    #[test]
    fn parse_config_lines_variants() {
        let parsed = parse_config_lines(
            "# comment\n\nopenrouter_api_key = \"sk-or-abc\"\nmodel_base = 'qwen/qwen3-coder'\nmodel_plan=openai/gpt-5\nbroken line\n",
        );
        assert_eq!(
            file_value(&parsed, "openrouter_api_key").as_deref(),
            Some("sk-or-abc")
        );
        assert_eq!(
            file_value(&parsed, "model_base").as_deref(),
            Some("qwen/qwen3-coder")
        );
        assert_eq!(
            file_value(&parsed, "model_plan").as_deref(),
            Some("openai/gpt-5")
        );
        assert_eq!(file_value(&parsed, "broken line"), None);
    }

    #[test]
    fn pricing_overrides_keep_quoted_model_keys_and_comments() {
        let prices = parse_pricing_overrides(r#"
input = 999
[pricing] # USD per million tokens
"openai::gpt-4.1-mini" = { input = 0.4, output = 1.6, cached_input = 0.1 } # published price
'groq::openai/gpt-oss-120b' = { output = 0.75, input = 0.15 }
"custom::model.#v1=beta" = { input = 0, output = 2 }
[mcp]
"openai::gpt-4.1-mini" = { input = 99, output = 99 }
"#);
        assert_eq!(prices.len(), 3);
        assert_eq!(prices["openai::gpt-4.1-mini"], (0.4, 1.6, Some(0.1)));
        assert_eq!(prices["groq::openai/gpt-oss-120b"], (0.15, 0.75, None));
        assert_eq!(prices["custom::model.#v1=beta"], (0.0, 2.0, None));
    }

    #[test]
    fn pricing_overrides_ignore_invalid_prices() {
        for entry in [
            r#""openai::m" = { input = -1, output = 2 }"#,
            r#""openai::m" = { input = 1, output = NaN }"#,
            r#""openai::m" = { input = inf, output = 2 }"#,
            r#""openai::m" = { input = 1, output = 2, cached_input = -1 }"#,
            r#""openai::m" = { input = 1, output = 2, cached_input = inf }"#,
            r#""openai::m" = { input = 1 }"#,
            r#""openai::m" = { output = 2 }"#,
            r#""openai::m" = { input = 1, output = 2, input = 3 }"#,
            r#""openai::m" = { input = 1, output = 2, typo = 3 }"#,
            r#""openai::m" = { input = 1,, output = 2 }"#,
            r#""openai::m" = { input = 1, output = 2, }"#,
            r#""openai::m" = { input = "1", output = 2 }"#,
            r#""unknown::m" = { input = 1, output = 2 }"#,
            r#""openai::" = { input = 1, output = 2 }"#,
            r#""m" = { input = 1, output = 2 }"#,
            r#"openai::m = { input = 1, output = 2 }"#,
        ] {
            assert!(parse_pricing_overrides(&format!("[pricing]\n{entry}")).is_empty(), "{entry}");
        }
    }

    #[test]
    fn pricing_overrides_read_the_selected_config_directory() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("SLAG_CONFIG_DIR");
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(dir.path().join("config.toml"), r#"
[pricing]
"openai::gpt-4.1-mini" = { input = 0.25, output = 0.75 }
"cerebras::gpt-4.1-mini" = { input = 1, output = 2 }
"#).unwrap();
        let prices = pricing_overrides();
        if let Some(previous) = previous {
            std::env::set_var("SLAG_CONFIG_DIR", previous);
        } else {
            std::env::remove_var("SLAG_CONFIG_DIR");
        }
        assert_eq!(prices["openai::gpt-4.1-mini"], (0.25, 0.75, None));
        assert_eq!(prices["cerebras::gpt-4.1-mini"], (1.0, 2.0, None));
    }

    #[test]
    fn pricing_overrides_survive_key_storage_and_removal() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("SLAG_CONFIG_DIR");
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(dir.path().join("config.toml"), r#"
openrouter_api_key = "old-key"
[pricing] # USD per million
"openai::gpt-4.1-mini" = { input = 0.4, output = 1.6, cached_input = 0.1 } # cached rate
'custom::model.#v1=beta' = { input = 0, output = 2 }
"#).unwrap();
        let expected = pricing_overrides();
        assert_eq!(expected.len(), 2);
        for provider in [ProviderPreset::OpenRouter, ProviderPreset::Cerebras] {
            store_key_for_provider(provider, "replacement-key").unwrap();
            assert_eq!(pricing_overrides(), expected);
            let contents = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
            assert!(contents.contains("pricing.\"openai::gpt-4.1-mini\" = { input"));
        }
        clear_key().unwrap();
        assert_eq!(pricing_overrides(), expected);
        let contents = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!contents.contains("openrouter_api_key"));
        assert!(contents.contains("cerebras_api_key"));
        if let Some(previous) = previous {
            std::env::set_var("SLAG_CONFIG_DIR", previous);
        } else {
            std::env::remove_var("SLAG_CONFIG_DIR");
        }
    }

    #[test]
    fn section_headers_namespace_their_keys() {
        let parsed = parse_config_lines(
            "model_base = qwen/qwen3-coder\n[mcp]\nfilesystem = \"npx -y server-fs /tmp\"\nmodel_base = shadowed\n",
        );
        // Top-level lookups ignore anything under a section header.
        assert_eq!(
            file_value(&parsed, "model_base").as_deref(),
            Some("qwen/qwen3-coder")
        );
        assert_eq!(
            file_value(&parsed, "mcp.filesystem").as_deref(),
            Some("npx -y server-fs /tmp")
        );
        assert_eq!(
            file_value(&parsed, "mcp.model_base").as_deref(),
            Some("shadowed")
        );
    }

    #[test]
    fn mcp_servers_read_the_mcp_table_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "openrouter_api_key = \"sk-or-x\"\nmodel_base = m\n\n[mcp]\nfs = \"npx -y server-fs /tmp\"\ngithub = 'gh-mcp --stdio'\n",
        )
        .unwrap();

        let servers = mcp_servers();
        assert_eq!(
            servers,
            vec![
                ("fs".to_string(), "npx -y server-fs /tmp".to_string()),
                ("github".to_string(), "gh-mcp --stdio".to_string()),
            ]
        );
        // The flat settings still load with the table present.
        let config = EngineConfig::load().expect("key stored in file");
        assert_eq!(config.api_key, "sk-or-x");
        assert_eq!(config.model_base, "m");

        clear_engine_env();
    }

    #[test]
    fn policy_entries_read_the_policy_table_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "model_base = m\n\n[policy]\ndeny = \"git push:*, curl:*\"\nask = 'cargo publish:*'\n",
        )
        .unwrap();

        let entries = policy_entries();
        assert_eq!(
            entries,
            vec![
                ("deny".to_string(), "git push:*, curl:*".to_string()),
                ("ask".to_string(), "cargo publish:*".to_string()),
            ]
        );

        clear_engine_env();
    }

    #[test]
    fn sandbox_mode_takes_env_over_file_and_stays_on_for_typos() {
        let some = |v: &str| Some(v.to_string());
        assert_eq!(sandbox_mode_from(None, None), SandboxMode::Auto);
        assert_eq!(sandbox_mode_from(None, some("off")), SandboxMode::Off);
        assert_eq!(sandbox_mode_from(some("auto"), some("off")), SandboxMode::Auto);
        assert_eq!(sandbox_mode_from(some("OFF"), some("auto")), SandboxMode::Off);
        assert_eq!(sandbox_mode_from(some("of"), None), SandboxMode::Auto);
    }

    #[test]
    fn sandbox_settings_read_mode_and_extra_paths() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        std::env::remove_var("SLAG_SANDBOX");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "sandbox = \"off\"\nsandbox_writable = [\"~/data\", '/srv/scratch']\n",
        )
        .unwrap();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());

        let (mode, extras) = sandbox_settings();
        assert_eq!(mode, SandboxMode::Off);
        assert_eq!(extras, vec![home.join("data"), PathBuf::from("/srv/scratch")]);

        // `--no-sandbox` and SLAG_SANDBOX travel the same env path.
        std::env::set_var("SLAG_SANDBOX", "auto");
        assert_eq!(sandbox_settings().0, SandboxMode::Auto);
        std::env::remove_var("SLAG_SANDBOX");
        clear_engine_env();
    }

    #[test]
    fn mcp_servers_empty_without_a_table() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(dir.path().join("config.toml"), "model_base = m\n").unwrap();
        assert!(mcp_servers().is_empty());
        clear_engine_env();
    }

    #[test]
    fn an_unset_alt_model_follows_the_base_model_and_keeps_auto_to_one_cast() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-x");
        std::env::set_var("SLAG_MODEL_BASE", "qwen/qwen3-coder");

        let config = EngineConfig::load().unwrap();
        assert_eq!(config.model_alt, "qwen/qwen3-coder");
        let ingot = casts_ingot(true, 5, "rebuild the engine", "cargo test");
        assert_eq!(config.casts_for(&ingot), 1);

        std::env::set_var("SLAG_MODEL_ALT", "other/alt-model");
        let config = EngineConfig::load().unwrap();
        assert_eq!(config.model_alt, "other/alt-model");
        assert_eq!(config.casts_for(&ingot), 3, "an explicit alt opens the duel");

        clear_engine_env();
    }

    #[test]
    fn store_key_then_load_round_trip() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        let path = store_key("sk-or-file-key").unwrap();
        assert_eq!(path, dir.path().join("config.toml"));

        let config = EngineConfig::load().expect("key stored in file");
        assert_eq!(config.api_key, "sk-or-file-key");
        assert_eq!(config.model_base, AUTO_MODEL);
        assert_eq!(config.model_plan, AUTO_MODEL);
        assert_eq!(config.model_alt, AUTO_MODEL);
        assert_eq!(config.model_judge, AUTO_MODEL);
        assert_eq!(config.effort, None);
        assert_eq!(config.base_url, crate::engine::OPENROUTER_BASE);
        assert_eq!(config.duel, DuelMode::Auto);
        assert_eq!(config.duel_rounds_override, None);
        assert_eq!(config.screenshot_cmd, None);

        clear_engine_env();
    }

    #[test]
    fn env_overrides_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "openrouter_api_key = \"sk-or-from-file\"\nmodel_base = \"file/base-model\"\n",
        )
        .unwrap();

        std::env::set_var("OPENROUTER_API_KEY", "sk-or-from-env");
        std::env::set_var("SLAG_MODEL_BASE", "env/base-model");
        std::env::set_var("SLAG_REASONING_EFFORT", "high");
        std::env::set_var("SLAG_OPENROUTER_BASE", "http://localhost:9999/v1");

        let config = EngineConfig::load().unwrap();
        assert_eq!(config.api_key, "sk-or-from-env");
        assert_eq!(config.model_base, "env/base-model");
        assert_eq!(config.model_plan, AUTO_MODEL);
        assert_eq!(config.effort, Some(Effort::High));
        assert_eq!(config.base_url, "http://localhost:9999/v1");

        clear_engine_env();
    }

    #[test]
    fn load_returns_none_without_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        assert!(EngineConfig::load().is_none());

        clear_engine_env();
    }

    #[test]
    fn invalid_effort_maps_to_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-x");
        std::env::set_var("SLAG_REASONING_EFFORT", "extreme");

        let config = EngineConfig::load().unwrap();
        assert_eq!(config.effort, None);

        clear_engine_env();
    }

    fn test_config() -> EngineConfig {
        EngineConfig {
            api_key: "sk-or-x".into(),
            model_base: "base-model".into(),
            model_plan: "plan-model".into(),
            model_alt: AUTO_MODEL.into(),
            model_judge: AUTO_MODEL.into(),
            effort: None,
            base_url: crate::engine::OPENROUTER_BASE.into(),
            duel: DuelMode::Auto,
            duel_rounds_override: None,
            screenshot_cmd: None,
        }
    }

    #[test]
    fn model_for_grade_switches_at_high_grade() {
        let config = test_config();
        assert_eq!(config.model_for_grade(1), "base-model");
        assert_eq!(config.model_for_grade(HIGH_GRADE - 1), "base-model");
        assert_eq!(config.model_for_grade(HIGH_GRADE), "plan-model");
        assert_eq!(config.model_for_grade(5), "plan-model");
    }

    #[test]
    fn duel_env_vars_load() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-x");
        std::env::set_var("SLAG_MODEL_ALT", "env/alt-model");
        std::env::set_var("SLAG_MODEL_JUDGE", "env/judge-model");
        std::env::set_var("SLAG_DUEL", "ON");
        std::env::set_var("SLAG_DUEL_ROUNDS", "7");
        std::env::set_var("SLAG_SCREENSHOT_CMD", "shot-scraper shot");

        let config = EngineConfig::load().unwrap();
        assert_eq!(config.model_alt, "env/alt-model");
        assert_eq!(config.model_judge, "env/judge-model");
        assert_eq!(config.duel, DuelMode::On);
        assert_eq!(config.duel_rounds_override, Some(7));
        assert_eq!(config.screenshot_cmd.as_deref(), Some("shot-scraper shot"));

        clear_engine_env();
    }

    #[test]
    fn alt_and_judge_models_load_from_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "openrouter_api_key = \"sk-or-file\"\nmodel_alt = \"file/alt\"\nmodel_judge = \"file/judge\"\n",
        )
        .unwrap();

        let config = EngineConfig::load().unwrap();
        assert_eq!(config.model_alt, "file/alt");
        assert_eq!(config.model_judge, "file/judge");

        clear_engine_env();
    }

    #[test]
    fn invalid_duel_value_maps_to_auto() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-x");
        std::env::set_var("SLAG_DUEL", "sometimes");
        std::env::set_var("SLAG_DUEL_ROUNDS", "many");

        let config = EngineConfig::load().unwrap();
        assert_eq!(config.duel, DuelMode::Auto);
        assert_eq!(config.duel_rounds_override, None);

        clear_engine_env();
    }


    #[test]
    fn duel_qualifies_by_mode_and_grade() {
        let mut config = test_config();
        assert!(!config.duel_qualifies(2));
        assert!(config.duel_qualifies(3));
        assert!(config.duel_qualifies(4));
        assert!(config.duel_qualifies(5));

        config.duel = DuelMode::On;
        assert!(config.duel_qualifies(1));

        config.duel = DuelMode::Off;
        assert!(!config.duel_qualifies(5));
    }

    #[test]
    fn duel_rounds_by_grade_with_override() {
        let mut config = test_config();
        assert_eq!(config.duel_rounds(3), 3);
        assert_eq!(config.duel_rounds(4), 3);
        assert_eq!(config.duel_rounds(5), 10);
        assert_eq!(config.duel_rounds(6), 10);

        config.duel_rounds_override = Some(4);
        assert_eq!(config.duel_rounds(4), 4);
        assert_eq!(config.duel_rounds(5), 4);
    }

    #[test]
    fn duel_keys_load_from_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "openrouter_api_key = \"sk-or-file\"\nduel = \"off\"\nduel_rounds = \"1\"\nscreenshot_cmd = \"shot {dir}\"\n",
        )
        .unwrap();

        let config = EngineConfig::load().unwrap();
        assert_eq!(config.duel, DuelMode::Off);
        assert_eq!(config.duel_rounds_override, Some(1));
        assert_eq!(config.screenshot_cmd.as_deref(), Some("shot {dir}"));

        // Env still overrides the file.
        std::env::set_var("SLAG_DUEL", "on");
        let config = EngineConfig::load().unwrap();
        assert_eq!(config.duel, DuelMode::On);

        clear_engine_env();
    }

    #[cfg(unix)]
    #[test]
    fn store_key_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        let path = store_key("sk-or-perm").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        clear_engine_env();
    }

    /// The whole point of the OpenRouter-only rewrite: a bare key is the
    /// only setup step. Every role must fall back to the auto router, and
    /// the literal id is pinned so a rename cannot slip through silently.
    #[test]
    fn every_model_role_defaults_to_auto_router() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-only-a-key");

        assert_eq!(AUTO_MODEL, "openrouter/auto");
        let config = EngineConfig::load().expect("key in env is enough");
        for (role, model) in [
            ("base", &config.model_base),
            ("plan", &config.model_plan),
            ("alt", &config.model_alt),
            ("judge", &config.model_judge),
        ] {
            assert_eq!(model, AUTO_MODEL, "role {role} should default to auto");
        }

        clear_engine_env();
    }

    #[test]
    fn mask_key_never_returns_the_raw_key() {
        let key = "sk-or-v1-0123456789abcdef0123456789";
        let masked = mask_key(key);
        assert_ne!(masked, key);
        assert!(masked.starts_with("sk-or-v"), "masked: {masked}");
        assert!(masked.ends_with("6789"), "masked: {masked}");
        assert!(masked.contains('…'), "masked: {masked}");
        // The middle is what a screenshot must not leak.
        assert!(!masked.contains(&key[7..key.len() - 4]), "masked: {masked}");
        assert!(masked.chars().count() < key.chars().count());

        // Short keys have no safe middle to show, so nothing is shown.
        for short in ["sk-or", "sk-or-v1-abcd", "sk-or-v1-abcde"] {
            let masked = mask_key(short);
            assert_ne!(masked, short);
            assert!(
                masked.chars().all(|c| c == '•'),
                "short key leaked: {masked}"
            );
            assert_eq!(masked.chars().count(), short.chars().count());
        }

        // Never returns an empty string, which would render as a blank panel.
        assert_eq!(mask_key(""), "•");
    }

    #[test]
    fn key_status_prefers_env_then_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        // Nothing anywhere.
        assert!(key_status().is_none());

        // File only.
        std::fs::write(
            dir.path().join("config.toml"),
            "openrouter_api_key = \"sk-or-from-file\"\n",
        )
        .unwrap();
        assert_eq!(
            key_status(),
            Some((KeySource::File, "sk-or-from-file".to_string()))
        );

        // Env wins, so CI can override a stored key.
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-from-env");
        assert_eq!(
            key_status(),
            Some((KeySource::Env, "sk-or-from-env".to_string()))
        );

        // A blank export is not a key: fall through to the file rather than
        // reporting an unusable empty one.
        std::env::set_var("OPENROUTER_API_KEY", "   ");
        assert_eq!(
            key_status(),
            Some((KeySource::File, "sk-or-from-file".to_string()))
        );

        assert_eq!(KeySource::Env.label(), "OPENROUTER_API_KEY");
        assert_eq!(KeySource::File.label(), "config file");

        clear_engine_env();
    }

    #[test]
    fn clear_key_removes_only_the_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        // No config file yet: forgetting nothing is not an error.
        assert_eq!(clear_key().unwrap(), None);

        std::fs::write(
            dir.path().join("config.toml"),
            "openrouter_api_key = \"sk-or-old\"\nmodel_base = \"kept/model\"\nduel = \"off\"\n",
        )
        .unwrap();

        let path = clear_key().unwrap().expect("config file existed");
        let parsed = parse_config_lines(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(file_value(&parsed, "openrouter_api_key"), None);
        assert_eq!(file_value(&parsed, "model_base").as_deref(), Some("kept/model"));
        assert_eq!(file_value(&parsed, "duel").as_deref(), Some("off"));
        assert!(EngineConfig::load().is_none(), "key should be gone");

        clear_engine_env();
    }

    #[tokio::test]
    async fn verify_and_store_saves_only_a_key_openrouter_accepts() {
        use wiremock::matchers::{method, path as req_path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(req_path("/key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": []
            })))
            .mount(&server)
            .await;
        std::env::set_var("SLAG_OPENROUTER_BASE", server.uri());
        assert_eq!(base_url(), server.uri());

        let stored = verify_and_store("sk-or-good").await.unwrap();
        assert_eq!(stored, dir.path().join("config.toml"));
        assert_eq!(
            key_status(),
            Some((KeySource::File, "sk-or-good".to_string()))
        );

        // A refused key must never reach disk: the old one stays, and a
        // fresh config dir stays empty.
        let refuser = MockServer::start().await;
        Mock::given(method("GET"))
            .and(req_path("/key"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&refuser)
            .await;
        std::env::set_var("SLAG_OPENROUTER_BASE", refuser.uri());

        let empty = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", empty.path());
        let err = verify_and_store("sk-or-bad").await.unwrap_err();
        assert!(err.to_string().contains("401"), "error: {err}");
        assert!(!empty.path().join("config.toml").exists());
        assert!(key_status().is_none());

        clear_engine_env();
    }

    /// Matched models keep an Auto duel shut: it spent about 60% more and
    /// changed no outcome. A distinct `model_alt` opens it by grade, and
    /// `On` duels matched models.
    #[test]
    fn auto_duel_needs_distinct_models() {
        let mut config = test_config();
        config.model_alt = config.model_base.clone();
        assert!(!config.duel_qualifies(5));
        assert!(!config.duel_qualifies(HIGH_GRADE));

        config.model_alt = "other/alt-model".into();
        assert!(config.duel_qualifies(5));
        assert!(config.duel_qualifies(HIGH_GRADE));
        assert!(!config.duel_qualifies(HIGH_GRADE - 1));

        config.model_alt = config.model_base.clone();
        config.duel = DuelMode::On;
        assert!(config.duel_qualifies(1));
    }

    fn casts_ingot(solo: bool, grade: u8, work: &str, proof: &str) -> Ingot {
        Ingot {
            id: "ix".into(),
            status: crate::sexp::Status::Ore,
            solo,
            grade,
            skill: crate::sexp::Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: proof.into(),
            bar: String::new(),
            work: work.into(),
            duel: None,
            casts: None,
            extra: vec![],
        }
    }

    #[test]
    fn casts_for_heuristic_matrix() {
        let config = test_config(); // DuelMode::Auto
        // (solo, grade, work, proof) -> expected casts
        let cases: &[(bool, u8, &str, &str, u8)] = &[
            // Grade gates: <=2 mechanical territory, 3-4 design middle, 5 studio.
            (true, 1, "wire the retry loop", "cargo test", 1),
            (true, 2, "wire the retry loop", "cargo test", 1),
            (true, 3, "choose the API shape", "cargo test", 2),
            (true, 4, "refactor the scheduler", "cargo test", 2),
            (true, 5, "rebuild the engine", "cargo test", 3),
            // Mechanical work drags a grade 3-4 down to one cast.
            (true, 4, "Create the config loader", "cargo test", 1),
            (true, 3, "rename the module", "cargo test", 1),
            // A deterministic proof does the same.
            (true, 4, "wire the loader", "test -f src/loader.rs", 1),
            (true, 3, "wire the loader", "grep -q loader src/lib.rs", 1),
            // Taste-dominant work goes to three casts at any grade.
            (true, 2, "polish the landing page", "true", 3),
            (true, 4, "visual design pass on the dashboard", "true", 3),
            // Sequential work never fans out.
            (false, 5, "rebuild the engine", "cargo test", 1),
        ];
        for &(solo, grade, work, proof, expected) in cases {
            let ingot = casts_ingot(solo, grade, work, proof);
            assert_eq!(
                config.casts_for(&ingot),
                expected,
                "solo {solo} grade {grade} work {work:?} proof {proof:?}"
            );
        }
    }

    #[test]
    fn casts_for_auto_gives_one_cast_when_the_models_match() {
        let mut config = test_config(); // DuelMode::Auto
        config.model_alt = config.model_base.clone();
        let studio = casts_ingot(true, 5, "rebuild the engine", "cargo test");
        let polish = casts_ingot(true, 2, "polish the landing page", "true");
        assert_eq!(config.casts_for(&studio), 1);
        assert_eq!(config.casts_for(&polish), 1);

        config.model_alt = "other/alt-model".into();
        assert_eq!(config.casts_for(&studio), 3, "a distinct alt model restores the heuristics");

        config.model_alt = config.model_base.clone();
        let mut pinned = studio.clone();
        pinned.casts = Some(2);
        assert_eq!(config.casts_for(&pinned), 2, "a pin keeps its meaning");
        config.duel = DuelMode::On;
        assert_eq!(config.casts_for(&studio), 3, "on keeps its meaning");
    }

    #[test]
    fn casts_for_explicit_pin_wins_over_everything() {
        let mut config = test_config();
        let mut ingot = casts_ingot(true, 1, "wire the retry loop", "test -f x");
        ingot.casts = Some(3);
        assert_eq!(config.casts_for(&ingot), 3, "pin beats heuristics");

        config.duel = DuelMode::Off;
        assert_eq!(config.casts_for(&ingot), 1, "SLAG_DUEL=off beats a pin");

        ingot.casts = Some(1);
        config.duel = DuelMode::On;
        assert_eq!(config.casts_for(&ingot), 1, "pin beats SLAG_DUEL=on");

        // The sequential gate is the one thing a pin cannot override.
        ingot.casts = Some(3);
        ingot.solo = false;
        assert_eq!(config.casts_for(&ingot), 1, "sequential never fans out");
    }

    #[test]
    fn casts_for_mode_and_legacy_duel_overrides() {
        let mut config = test_config();
        let design = casts_ingot(true, 4, "refactor the scheduler", "cargo test");
        let simple = casts_ingot(true, 1, "wire the retry loop", "cargo test");

        config.duel = DuelMode::Off;
        assert_eq!(config.casts_for(&design), 1, "off forces one cast");

        config.duel = DuelMode::On;
        assert_eq!(config.casts_for(&simple), 2, "on forces at least two");
        let studio = casts_ingot(true, 5, "rebuild the engine", "cargo test");
        assert_eq!(config.casts_for(&studio), 3, "on keeps the 3-cast tier");

        // Legacy :duel override sits between the pin and the mode, but
        // off is the kill switch.
        config.duel = DuelMode::Off;
        let mut forced = simple.clone();
        forced.duel = Some(true);
        assert_eq!(config.casts_for(&forced), 1, "off beats :duel t");
        config.duel = DuelMode::Auto;
        assert_eq!(config.casts_for(&forced), 2, ":duel t beats auto");
        config.duel = DuelMode::On;
        let mut blocked = design.clone();
        blocked.duel = Some(false);
        assert_eq!(config.casts_for(&blocked), 1, ":duel nil beats on");
    }

    #[test]
    fn cost_caps_default_to_uncapped() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        assert_eq!(ingot_cost_cap(), None);
        assert_eq!(run_cost_cap(), None);

        clear_engine_env();
    }

    #[test]
    fn cost_caps_env_overrides_file_and_rejects_garbage() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "max_cost_per_ingot = \"1.50\"\nmax_cost_per_run = \"20\"\n",
        )
        .unwrap();

        // File keys load.
        assert_eq!(ingot_cost_cap(), Some(1.50));
        assert_eq!(run_cost_cap(), Some(20.0));

        // Env wins over file.
        std::env::set_var("SLAG_MAX_COST_INGOT", "0.75");
        std::env::set_var("SLAG_MAX_COST_RUN", "5.5");
        assert_eq!(ingot_cost_cap(), Some(0.75));
        assert_eq!(run_cost_cap(), Some(5.5));

        // Garbage, zero, and negative values mean uncapped, not a crash.
        for bad in ["lots", "0", "-3", "NaN"] {
            std::env::set_var("SLAG_MAX_COST_INGOT", bad);
            assert_eq!(ingot_cost_cap(), None, "value {bad:?} must not cap");
        }

        clear_engine_env();
    }

    #[test]
    fn fallback_model_env_overrides_file_and_defaults_off() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        assert_eq!(fallback_model(), None);

        std::fs::write(
            dir.path().join("config.toml"),
            "fallback_model = \"deepseek/deepseek-chat\"\n",
        )
        .unwrap();
        assert_eq!(fallback_model().as_deref(), Some("deepseek/deepseek-chat"));

        std::env::set_var("SLAG_MODEL_FALLBACK", "qwen/qwen3-coder");
        assert_eq!(fallback_model().as_deref(), Some("qwen/qwen3-coder"));

        // Blank env export falls through to the file, not to "".
        std::env::set_var("SLAG_MODEL_FALLBACK", "  ");
        assert_eq!(fallback_model().as_deref(), Some("deepseek/deepseek-chat"));

        clear_engine_env();
    }

    #[test]
    fn unattended_retry_parses_truthy_values_and_defaults_off() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        assert!(!unattended_retry());

        for on in ["1", "true", "ON", "yes"] {
            std::env::set_var("SLAG_UNATTENDED_RETRY", on);
            assert!(unattended_retry(), "value {on:?} should enable");
        }
        for off in ["0", "false", "off", "sometimes"] {
            std::env::set_var("SLAG_UNATTENDED_RETRY", off);
            assert!(!unattended_retry(), "value {off:?} should disable");
        }

        // File key works; env still wins.
        std::env::remove_var("SLAG_UNATTENDED_RETRY");
        std::fs::write(dir.path().join("config.toml"), "unattended_retry = \"on\"\n").unwrap();
        assert!(unattended_retry());
        std::env::set_var("SLAG_UNATTENDED_RETRY", "off");
        assert!(!unattended_retry());

        clear_engine_env();
    }

    #[test]
    fn parse_truthy_covers_the_edge_spellings() {
        assert!(parse_truthy(Some(" True ".into())));
        assert!(!parse_truthy(Some("".into())));
        assert!(!parse_truthy(None));
    }

    #[test]
    fn millicent_conversion_round_trips_and_clamps() {
        assert_eq!(dollars_to_millicents(0.0), 0);
        assert_eq!(dollars_to_millicents(1.0), 100_000);
        assert_eq!(dollars_to_millicents(0.00001), 1);
        // Negative cost (bad provider data) never underflows.
        assert_eq!(dollars_to_millicents(-2.0), 0);
        let d = millicents_to_dollars(dollars_to_millicents(12.34567));
        assert!((d - 12.34567).abs() < 1e-5, "round trip drifted: {d}");
    }

    #[test]
    fn run_spend_accumulates_monotonically() {
        // The accumulator is process-global and other tests may add to it
        // concurrently, so assert on the delta, not the absolute value.
        let before = run_spend_dollars();
        add_run_spend(1.25);
        add_run_spend(-5.0); // ignored, never subtracts
        add_run_spend(0.75);
        let after = run_spend_dollars();
        assert!(after - before >= 2.0 - 1e-9, "before {before}, after {after}");
    }

    #[test]
    fn store_key_preserves_other_entries() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());
        std::fs::write(
            dir.path().join("config.toml"),
            "openrouter_api_key = \"sk-or-old\"\nmodel_base = \"kept/model\"\n",
        )
        .unwrap();

        let path = store_key("sk-or-new").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed = parse_config_lines(&contents);
        assert_eq!(
            file_value(&parsed, "openrouter_api_key").as_deref(),
            Some("sk-or-new")
        );
        assert_eq!(
            file_value(&parsed, "model_base").as_deref(),
            Some("kept/model")
        );

        clear_engine_env();
    }
}

#[cfg(test)]
mod project_paths_tests {
    use super::*;

    #[test]
    fn new_layout_resolves_coordination_and_logs_under_slag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".slag")).unwrap();
        for name in [ORE_FILE, BLUEPRINT, CRUCIBLE, ALLOY_FILE, LEDGER, "BAR.md", LOG_DIR] {
            assert_eq!(project_path_in(dir.path(), name), dir.path().join(".slag").join(name));
        }
    }

    #[test]
    fn layout_marker_keeps_new_paths_after_a_root_plan_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".slag")).unwrap();
        std::fs::write(dir.path().join(".slag/layout"), "1\n").unwrap();
        std::fs::write(dir.path().join(CRUCIBLE), "stale root plan").unwrap();
        assert_eq!(project_path_in(dir.path(), CRUCIBLE), dir.path().join(".slag/PLAN.md"));
        assert_eq!(project_path_in(dir.path(), LOG_DIR), dir.path().join(".slag/logs"));
    }

    #[test]
    fn root_plan_or_ore_keeps_legacy_layout_with_existing_slag_directory() {
        for marker in [CRUCIBLE, ORE_FILE] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join(".slag")).unwrap();
            std::fs::write(dir.path().join(marker), "legacy").unwrap();
            for name in [ORE_FILE, BLUEPRINT, CRUCIBLE, ALLOY_FILE, LEDGER, "BAR.md", LOG_DIR] {
                assert_eq!(project_path_in(dir.path(), name), dir.path().join(name));
            }
        }
    }
}
