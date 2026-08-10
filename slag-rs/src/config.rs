use std::io::IsTerminal;
use std::path::PathBuf;

use crate::engine::Effort;
use crate::error::SlagError;

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

/// Smith configuration resolved from environment
pub struct SmithConfig {
    pub base: String,
    pub plan: String,
    pub web: String,
    pub web_plan: String,
}

impl SmithConfig {
    pub fn from_env() -> Self {
        let base = std::env::var("SLAG_SMITH")
            .unwrap_or_else(|_| "claude --dangerously-skip-permissions -p".to_string());
        let plan = format!("{base} --permission-mode plan");
        let web = format!("{base} --allowedTools 'Bash Edit Read Write Playwright'");
        let web_plan = format!("{web} --permission-mode plan");
        Self {
            base,
            plan,
            web,
            web_plan,
        }
    }

    /// Select smith command based on skill and grade
    pub fn select(&self, skill: &str, grade: u8) -> &str {
        match skill {
            "web" | "frontend" | "ui" | "css" | "html" => {
                if grade >= HIGH_GRADE {
                    &self.web_plan
                } else {
                    &self.web
                }
            }
            _ => {
                if grade >= HIGH_GRADE {
                    &self.plan
                } else {
                    &self.base
                }
            }
        }
    }
}

/// Resolve a project-relative path
pub fn project_path(filename: &str) -> PathBuf {
    PathBuf::from(filename)
}

/// Default models for the native engine
const DEFAULT_MODEL_BASE: &str = "qwen/qwen3-coder";
const DEFAULT_MODEL_PLAN: &str = "openai/gpt-5";
const DEFAULT_MODEL_ALT: &str = "moonshotai/kimi-k2";
const DEFAULT_MODEL_JUDGE: &str = "openai/gpt-5";

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
    /// Cast B model — different family than base, forced diversity.
    pub model_alt: String,
    /// Assayer model — different family than both smiths.
    pub model_judge: String,
    pub effort: Option<Effort>,
    pub base_url: String,
    pub duel: DuelMode,
    pub duel_rounds_override: Option<u8>,
    /// Shell command producing a screenshot for visual assay (web ingots).
    pub screenshot_cmd: Option<String>,
}

impl EngineConfig {
    /// Resolve engine config. Returns None when no API key is available
    /// anywhere (caller falls back to the CLI smith or onboarding).
    pub fn load() -> Option<Self> {
        let entries = config_file_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|c| parse_config_lines(&c))
            .unwrap_or_default();

        let api_key = env_nonempty("OPENROUTER_API_KEY")
            .or_else(|| file_value(&entries, "openrouter_api_key"))?;

        let model_base = env_nonempty("SLAG_MODEL_BASE")
            .or_else(|| file_value(&entries, "model_base"))
            .unwrap_or_else(|| DEFAULT_MODEL_BASE.to_string());

        let model_plan = env_nonempty("SLAG_MODEL_PLAN")
            .or_else(|| file_value(&entries, "model_plan"))
            .unwrap_or_else(|| DEFAULT_MODEL_PLAN.to_string());

        let model_alt = env_nonempty("SLAG_MODEL_ALT")
            .or_else(|| file_value(&entries, "model_alt"))
            .unwrap_or_else(|| DEFAULT_MODEL_ALT.to_string());

        let model_judge = env_nonempty("SLAG_MODEL_JUDGE")
            .or_else(|| file_value(&entries, "model_judge"))
            .unwrap_or_else(|| DEFAULT_MODEL_JUDGE.to_string());

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
    /// grade 3 and above.
    pub fn duel_qualifies(&self, grade: u8) -> bool {
        match self.duel {
            DuelMode::On => true,
            DuelMode::Off => false,
            DuelMode::Auto => grade >= HIGH_GRADE,
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

    let mut out = String::new();
    for (k, v) in &entries {
        out.push_str(k);
        out.push_str(" = \"");
        out.push_str(v);
        out.push_str("\"\n");
    }
    std::fs::write(&path, out)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(path)
}

/// Interactive key onboarding. Only prompts on a real terminal;
/// headless runs get a hard config error instead of hanging on stdin.
pub fn prompt_for_key() -> Result<String, SlagError> {
    use std::io::{BufRead, Write};

    if !std::io::stdin().is_terminal() {
        return Err(SlagError::Config("OPENROUTER_API_KEY not set".into()));
    }

    println!("slag needs an OpenRouter key — get one at openrouter.ai/keys");
    print!("key: ");
    std::io::stdout().flush()?;

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err(SlagError::Config("empty OpenRouter key".into()));
    }

    store_key(&key)?;
    Ok(key)
}

/// Config directory: $SLAG_CONFIG_DIR override (tests), else ~/.config/slag.
fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SLAG_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("slag"))
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
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
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
            entries.push((key.to_string(), value.to_string()));
        }
    }
    entries
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
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    fn store_key_then_load_round_trip() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_engine_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SLAG_CONFIG_DIR", dir.path());

        let path = store_key("sk-or-file-key").unwrap();
        assert_eq!(path, dir.path().join("config.toml"));

        let config = EngineConfig::load().expect("key stored in file");
        assert_eq!(config.api_key, "sk-or-file-key");
        assert_eq!(config.model_base, DEFAULT_MODEL_BASE);
        assert_eq!(config.model_plan, DEFAULT_MODEL_PLAN);
        assert_eq!(config.model_alt, DEFAULT_MODEL_ALT);
        assert_eq!(config.model_judge, DEFAULT_MODEL_JUDGE);
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
        assert_eq!(config.model_plan, DEFAULT_MODEL_PLAN);
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
            model_alt: DEFAULT_MODEL_ALT.into(),
            model_judge: DEFAULT_MODEL_JUDGE.into(),
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
