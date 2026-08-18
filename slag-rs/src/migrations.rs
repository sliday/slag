//! migrations — idempotent startup fixups, run early in `main` before any
//! command dispatch.
//!
//! Two classes today: deprecated OpenRouter model slugs pinned in the
//! config file (OpenRouter retires slugs regularly, and a retired slug
//! 404s every request until someone edits the file by hand), and
//! bash-era crucible headers (`;; CRUCIBLE Tue Jan 27 10:13:45 CET 2026`
//! from slag.sh) upgraded to the ISO form the Rust writer emits.
//!
//! Each migration is a pure function over file contents that returns
//! `Some(new)` only when something actually changed, so a second run is a
//! no-op by construction — no completion flags, nothing to corrupt.
//! `run` itself is best-effort: a migration that cannot read or write its
//! file must never block a forge.

use std::path::{Path, PathBuf};

/// Retired OpenRouter slugs mapped to living successors in the same
/// family. Only exact matches rewrite; anything unrecognized is left for
/// the provider to reject with a real error message.
const SLUG_MAP: &[(&str, &str)] = &[
    ("openai/gpt-3.5-turbo", "openai/gpt-4o-mini"),
    ("openai/gpt-3.5-turbo-16k", "openai/gpt-4o-mini"),
    ("openai/gpt-4-vision-preview", "openai/gpt-4o"),
    ("anthropic/claude-2", "anthropic/claude-sonnet-4"),
    ("anthropic/claude-2.0", "anthropic/claude-sonnet-4"),
    ("anthropic/claude-2.1", "anthropic/claude-sonnet-4"),
    ("anthropic/claude-instant-1", "anthropic/claude-3.5-haiku"),
    ("anthropic/claude-instant-1.2", "anthropic/claude-3.5-haiku"),
    ("google/palm-2-chat-bison", "google/gemini-2.0-flash-001"),
];

/// The config keys that hold model slugs. Other keys (screenshot_cmd,
/// the API key) must never be touched even if their value collides.
const MODEL_KEYS: &[&str] = &["model_base", "model_plan", "model_alt", "model_judge"];

/// Run every migration. Silent when nothing needs doing; one dim line per
/// file actually rewritten. Never errors — a broken migration is a
/// skipped migration.
pub fn run() {
    if let Some(path) = config_file() {
        apply(&path, "model slugs", rewrite_model_slugs);
    }
    apply(
        Path::new(crate::config::CRUCIBLE),
        "crucible header",
        upgrade_crucible_header,
    );
}

/// Read → migrate → write, only touching disk when the pure function
/// reports a change.
fn apply(path: &Path, label: &str, migrate: fn(&str) -> Option<String>) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    if let Some(new) = migrate(&contents) {
        if std::fs::write(path, new).is_ok() {
            eprintln!(
                "  {}",
                crate::tui::dim(&format!("⚒ migrated {label} in {}", path.display()))
            );
        }
    }
}

/// Same resolution as `config.rs` (which keeps its path helper private):
/// `$SLAG_CONFIG_DIR/config.toml`, else `~/.config/slag/config.toml`.
fn config_file() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SLAG_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("config.toml"));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config").join("slag").join("config.toml"))
}

/// Rewrite deprecated slug values on the `model_*` keys. Returns `Some`
/// only when a line changed; comments, blanks, and every other key pass
/// through untouched.
pub fn rewrite_model_slugs(contents: &str) -> Option<String> {
    let mut changed = false;
    let lines: Vec<String> = contents
        .lines()
        .map(|line| rewrite_model_line(line, &mut changed))
        .collect();
    if !changed {
        return None;
    }
    let mut out = lines.join("\n");
    if contents.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

fn rewrite_model_line(line: &str, changed: &mut bool) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return line.to_string();
    }
    let Some((key, value)) = trimmed.split_once('=') else {
        return line.to_string();
    };
    let key = key.trim();
    if !MODEL_KEYS.contains(&key) {
        return line.to_string();
    }
    let value = unquote(value.trim());
    let Some((_, new)) = SLUG_MAP.iter().find(|(old, _)| *old == value) else {
        return line.to_string();
    };
    *changed = true;
    format!("{key} = \"{new}\"")
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Upgrade a bash-era crucible header timestamp (`;; CRUCIBLE Tue Jan 27
/// 10:13:45 CET 2026`, the `date` default from slag.sh) to the ISO form
/// the Rust writer emits (`;; CRUCIBLE 2026-01-27 10:13`). Idempotent:
/// the ISO form no longer matches the six-token bash shape. Anything
/// unparseable is left alone — a header is documentation, not data worth
/// destroying on a guess.
pub fn upgrade_crucible_header(contents: &str) -> Option<String> {
    const PREFIX: &str = ";; CRUCIBLE ";
    let mut changed = false;
    let lines: Vec<String> = contents
        .lines()
        .map(|line| {
            if changed {
                return line.to_string(); // only the first header rewrites
            }
            let Some(rest) = line.strip_prefix(PREFIX) else {
                return line.to_string();
            };
            let toks: Vec<&str> = rest.split_whitespace().collect();
            // Six tokens: Dow Mon Day HH:MM:SS TZ Year. chrono cannot parse
            // the timezone abbreviation, so drop it before parsing.
            if toks.len() != 6 {
                return line.to_string();
            }
            let candidate =
                format!("{} {} {} {} {}", toks[0], toks[1], toks[2], toks[3], toks[5]);
            match chrono::NaiveDateTime::parse_from_str(&candidate, "%a %b %d %H:%M:%S %Y") {
                Ok(dt) => {
                    changed = true;
                    format!("{PREFIX}{}", dt.format("%Y-%m-%d %H:%M"))
                }
                Err(_) => line.to_string(),
            }
        })
        .collect();
    if !changed {
        return None;
    }
    let mut out = lines.join("\n");
    if contents.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deprecated_slug_rewrites_only_model_keys() {
        let cfg = "openrouter_api_key = \"sk-or-x\"\n\
                   model_base = \"anthropic/claude-2\"\n\
                   screenshot_cmd = \"anthropic/claude-2\"\n";
        let out = rewrite_model_slugs(cfg).expect("should rewrite");
        assert!(out.contains("model_base = \"anthropic/claude-sonnet-4\""), "{out}");
        // The same string under a non-model key must survive untouched.
        assert!(out.contains("screenshot_cmd = \"anthropic/claude-2\""), "{out}");
        assert!(out.contains("openrouter_api_key = \"sk-or-x\""), "{out}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn slug_rewrite_is_idempotent() {
        let cfg = "model_plan = openai/gpt-3.5-turbo\n";
        let once = rewrite_model_slugs(cfg).expect("first pass rewrites");
        assert!(once.contains("model_plan = \"openai/gpt-4o-mini\""), "{once}");
        assert_eq!(rewrite_model_slugs(&once), None, "second pass must be a no-op");
    }

    #[test]
    fn living_slugs_and_comments_pass_through() {
        let cfg = "# model_base = \"anthropic/claude-2\"\n\
                   model_base = \"openrouter/auto\"\n\n\
                   not a kv line\n";
        assert_eq!(rewrite_model_slugs(cfg), None);
    }

    #[test]
    fn single_quoted_deprecated_slug_rewrites() {
        let cfg = "model_judge = 'anthropic/claude-instant-1'";
        let out = rewrite_model_slugs(cfg).expect("should rewrite");
        assert_eq!(out, "model_judge = \"anthropic/claude-3.5-haiku\"");
        assert!(!out.ends_with('\n'), "no trailing newline invented");
    }

    #[test]
    fn bash_era_header_upgrades_to_iso() {
        let plan = ";; CRUCIBLE Tue Jan 27 10:13:45 CET 2026\n\
                    ;; Blueprint: BLUEPRINT.md\n\
                    (ingot :id \"i1\" :status ore)\n";
        let out = upgrade_crucible_header(plan).expect("should upgrade");
        assert!(out.starts_with(";; CRUCIBLE 2026-01-27 10:13\n"), "{out}");
        assert!(out.contains(";; Blueprint: BLUEPRINT.md"), "{out}");
        assert!(out.contains("(ingot :id \"i1\""), "{out}");
    }

    #[test]
    fn header_upgrade_is_idempotent() {
        let plan = ";; CRUCIBLE Tue Jan 27 10:13:45 CET 2026\n";
        let once = upgrade_crucible_header(plan).expect("first pass upgrades");
        assert_eq!(upgrade_crucible_header(&once), None, "ISO header must not re-match");
    }

    #[test]
    fn unparseable_headers_are_left_alone() {
        // ISO form, garbage, and a headerless file all no-op.
        assert_eq!(upgrade_crucible_header(";; CRUCIBLE 2026-01-27 10:13\n"), None);
        assert_eq!(upgrade_crucible_header(";; CRUCIBLE not a date at all here yes\n"), None);
        assert_eq!(upgrade_crucible_header("(ingot :id \"i1\" :status ore)\n"), None);
    }

    #[test]
    fn single_digit_day_parses() {
        let plan = ";; CRUCIBLE Mon Feb 3 09:05:01 UTC 2025";
        let out = upgrade_crucible_header(plan).expect("should upgrade");
        assert_eq!(out, ";; CRUCIBLE 2025-02-03 09:05");
    }
}
