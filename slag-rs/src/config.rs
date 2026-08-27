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

/// Resolve a project-relative path
pub fn project_path(filename: &str) -> PathBuf {
    PathBuf::from(filename)
}

/// Default model for every role: OpenRouter's automatic router picks a
/// live model per request, so a fresh key works with zero model config.
/// Each role still takes an env/file/flag override.
pub const AUTO_MODEL: &str = "openrouter/auto";

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
    /// make a duel worth its cost; left at the default it matches base.
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
    /// Resolve config, onboarding the user when no key exists yet.
    /// The one prerequisite slag has: an OpenRouter key.
    pub async fn resolve() -> Result<Self, SlagError> {
        if let Some(cfg) = Self::load() {
            return Ok(cfg);
        }
        onboard().await?;
        Self::load().ok_or_else(|| SlagError::Config("key stored but config still unreadable".into()))
    }

    /// Resolve engine config. Returns None when no API key is available
    /// anywhere (caller onboards instead).
    pub fn load() -> Option<Self> {
        let entries = config_file_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|c| parse_config_lines(&c))
            .unwrap_or_default();

        let api_key = env_nonempty("OPENROUTER_API_KEY")
            .or_else(|| file_value(&entries, "openrouter_api_key"))?;

        let model_base = env_nonempty("SLAG_MODEL_BASE")
            .or_else(|| file_value(&entries, "model_base"))
            .unwrap_or_else(|| AUTO_MODEL.to_string());

        let model_plan = env_nonempty("SLAG_MODEL_PLAN")
            .or_else(|| file_value(&entries, "model_plan"))
            .unwrap_or_else(|| AUTO_MODEL.to_string());

        let model_alt = env_nonempty("SLAG_MODEL_ALT")
            .or_else(|| file_value(&entries, "model_alt"))
            .unwrap_or_else(|| AUTO_MODEL.to_string());

        let model_judge = env_nonempty("SLAG_MODEL_JUDGE")
            .or_else(|| file_value(&entries, "model_judge"))
            .unwrap_or_else(|| AUTO_MODEL.to_string());

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

        let base_url = env_nonempty("SLAG_OPENROUTER_BASE")
            .unwrap_or_else(|| crate::engine::OPENROUTER_BASE.to_string());

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

    /// Select model by ingot grade: high grades get the plan/reasoning model.
    pub fn model_for_grade(&self, grade: u8) -> &str {
        if grade >= HIGH_GRADE {
            &self.model_plan
        } else {
            &self.model_base
        }
    }

    /// Grade-gated duel qualification (plan section 9 rule 1): Auto duels
    /// grade 3 and above. Matched models no longer disqualify a duel: the
    /// opposing direction prompts (minimal / robust / creative) carry the
    /// diversity, so even two casts of `openrouter/auto` explore
    /// different solutions.
    pub fn duel_qualifies(&self, grade: u8) -> bool {
        match self.duel {
            DuelMode::On => true,
            DuelMode::Off => false,
            DuelMode::Auto => grade >= HIGH_GRADE,
        }
    }

    /// Resolve how many casts forge this ingot (adaptive cast count).
    ///
    /// Sequential ingots always get one cast — overlapping sequential
    /// work from multiple casts is a merge-conflict factory. After that
    /// gate: an explicit `:casts` pin wins, then the legacy `:duel`
    /// override, then `SLAG_DUEL` (off forces 1, on forces at least 2),
    /// then the work-shape heuristics. Crack-retry escalation (heat > 0
    /// bumps 1 → 2) lives in the forge scheduler, not here.
    pub fn casts_for(&self, ingot: &Ingot) -> u8 {
        if !ingot.solo {
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
    let dir = config_dir()
        .ok_or_else(|| SlagError::Config("cannot resolve config dir: $HOME not set".into()))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");

    let mut entries = std::fs::read_to_string(&path)
        .map(|c| parse_config_lines(&c))
        .unwrap_or_default();
    match entries.iter_mut().find(|(k, _)| k == "openrouter_api_key") {
        Some(entry) => entry.1 = key.to_string(),
        None => entries.push(("openrouter_api_key".to_string(), key.to_string())),
    }

    write_entries(&path, &entries)?;
    Ok(path)
}

/// Rewrite the config file from `key = "value"` pairs, owner-only on unix.
fn write_entries(path: &std::path::Path, entries: &[(String, String)]) -> Result<(), SlagError> {
    let mut out = String::new();
    for (k, v) in entries {
        out.push_str(k);
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
    pub fn label(self) -> &'static str {
        match self {
            KeySource::Env => "OPENROUTER_API_KEY",
            KeySource::File => "config file",
        }
    }
}

/// Current key and where it came from, without onboarding.
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
    env_nonempty("SLAG_OPENROUTER_BASE").unwrap_or_else(|| crate::engine::OPENROUTER_BASE.to_string())
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
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
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
    use std::sync::Mutex;

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
    ];

    fn clear_engine_env() {
        for var in ENGINE_VARS {
            std::env::remove_var(var);
        }
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

    /// Matched models no longer disqualify an Auto duel: the direction
    /// prompts (minimal / robust / creative) carry the diversity, so two
    /// casts of `openrouter/auto` still explore different solutions.
    #[test]
    fn auto_duel_no_longer_requires_distinct_models() {
        let mut config = test_config();
        config.model_alt = config.model_base.clone();
        assert!(config.duel_qualifies(5));
        assert!(config.duel_qualifies(HIGH_GRADE));
        assert!(!config.duel_qualifies(HIGH_GRADE - 1));

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
    fn casts_for_explicit_pin_wins_over_everything() {
        let mut config = test_config();
        let mut ingot = casts_ingot(true, 1, "wire the retry loop", "test -f x");
        ingot.casts = Some(3);
        assert_eq!(config.casts_for(&ingot), 3, "pin beats heuristics");

        config.duel = DuelMode::Off;
        assert_eq!(config.casts_for(&ingot), 3, "pin beats SLAG_DUEL=off");

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

        // Legacy :duel override sits between the pin and the mode.
        config.duel = DuelMode::Off;
        let mut forced = simple.clone();
        forced.duel = Some(true);
        assert_eq!(config.casts_for(&forced), 2, ":duel t beats off");
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
