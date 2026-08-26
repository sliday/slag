//! Command policy engine (item 95): config-driven allow/deny/ask rules
//! for bash commands, beyond the built-in destructive-command deny table.
//!
//! Rules live in the `[policy]` table of slag's config file:
//!
//! ```toml
//! [policy]
//! deny = ["git push:*", "curl:*"]
//! ask = ["cargo publish:*"]
//! allow = ["git push --dry-run:*"]
//! ```
//!
//! Semantics, mirroring Claude Code's bashPermissions but fail-closed:
//! compound commands are split on `&&`, `;`, `|`, `&`, and newlines and
//! every segment must pass; wrapper and env prefixes (`sudo`, `env X=1`,
//! `nice`, `time`, …) are stripped iteratively before matching; backtick
//! and `$( )` command substitution cannot be statically checked, so any
//! occurrence is refused outright while a policy is configured; rule
//! precedence is deny > ask > allow, with unmatched commands allowed.
//! An empty policy (no `[policy]` table) checks nothing.
//! Hand-rolled matching, no regex dependency (house convention).

/// Outcome of checking one command against the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Matched an `ask` rule. Slag runs unattended, so "ask" refuses with
    /// guidance to add an `allow` rule instead of prompting.
    Ask { rule: String },
    Deny { rule: String },
}

/// One parsed rule: token-wise command prefix, with `:*` meaning "any
/// further arguments". `"git push:*"` matches `git push`, `git push -f`,
/// … while bare `"git push"` matches only the exact two-token command.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    tokens: Vec<String>,
    wildcard: bool,
    /// The rule as written, for refusal messages.
    source: String,
}

impl Rule {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let (body, wildcard) = match raw.strip_suffix(":*") {
            Some(b) => (b, true),
            None => (raw, false),
        };
        let tokens: Vec<String> = body.split_whitespace().map(str::to_string).collect();
        if tokens.is_empty() {
            return None;
        }
        Some(Rule { tokens, wildcard, source: raw.to_string() })
    }

    /// Token-wise prefix match against a stripped segment.
    fn matches(&self, cmd: &[&str]) -> bool {
        if cmd.len() < self.tokens.len() {
            return false;
        }
        if !self.wildcard && cmd.len() != self.tokens.len() {
            return false;
        }
        self.tokens.iter().zip(cmd).all(|(pat, tok)| pat == tok)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Policy {
    deny: Vec<Rule>,
    ask: Vec<Rule>,
    allow: Vec<Rule>,
}

impl Policy {
    /// Build from `(key, value)` pairs as `config::policy_entries`
    /// returns them: keys `deny` / `ask` / `allow` (repeatable), values
    /// either a TOML-style array (`["a", "b"]`) or a comma-separated
    /// list. Unknown keys are ignored.
    pub fn from_entries(entries: &[(String, String)]) -> Self {
        let mut policy = Policy::default();
        for (key, value) in entries {
            let bucket = match key.as_str() {
                "deny" => &mut policy.deny,
                "ask" => &mut policy.ask,
                "allow" => &mut policy.allow,
                _ => continue,
            };
            bucket.extend(parse_rule_list(value));
        }
        policy
    }

    /// Load from the config file's `[policy]` table.
    pub fn from_config() -> Self {
        Self::from_entries(&crate::config::policy_entries())
    }

    pub fn is_empty(&self) -> bool {
        self.deny.is_empty() && self.ask.is_empty() && self.allow.is_empty()
    }

    /// Check a full command line. Every compound segment must pass.
    pub fn check(&self, command: &str) -> Decision {
        if self.is_empty() {
            return Decision::Allowed;
        }
        // Fail closed: `$(…)` and backticks run arbitrary commands the
        // matcher cannot see. While a policy is configured, refuse them.
        if command.contains('`') || command.contains("$(") {
            return Decision::Deny { rule: "command substitution".into() };
        }
        for seg in split_compound(command) {
            match self.check_segment(seg) {
                Decision::Allowed => {}
                other => return other,
            }
        }
        Decision::Allowed
    }

    /// One segment: strip wrappers, then deny > ask > allow.
    fn check_segment(&self, seg: &str) -> Decision {
        let toks: Vec<&str> = seg.split_whitespace().collect();
        let stripped = strip_wrappers(&toks);
        if stripped.is_empty() {
            return Decision::Allowed;
        }
        // `sh -c '<payload>'`: check the payload commands as well.
        if let Some(payload) = shell_c_payload(&stripped) {
            match self.check(&payload) {
                Decision::Allowed => {}
                other => return other,
            }
        }
        // Basename the command so `/usr/bin/curl` matches a `curl` rule.
        let mut cmd: Vec<&str> = stripped;
        let base = cmd[0].trim_start_matches('\\');
        let base = base.rsplit('/').next().unwrap_or(base);
        cmd[0] = base;
        for rule in &self.deny {
            if rule.matches(&cmd) {
                return Decision::Deny { rule: rule.source.clone() };
            }
        }
        for rule in &self.ask {
            if rule.matches(&cmd) {
                return Decision::Ask { rule: rule.source.clone() };
            }
        }
        // Allow rules exist for explicitness/documentation; unmatched
        // commands are allowed anyway (the policy is a guard rail, not
        // an allowlist).
        Decision::Allowed
    }
}

/// `["a", "b"]` or `a, b` → rules.
fn parse_rule_list(value: &str) -> Vec<Rule> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    inner
        .split(',')
        .map(|item| item.trim().trim_matches(|c| c == '"' || c == '\''))
        .filter_map(Rule::parse)
        .collect()
}

/// Split on compound operators so every subcommand is checked. Quote
/// state is not tracked (same tradeoff as the destructive-command
/// splitter): a quoted `|` splits too, which errs toward checking a
/// non-command fragment that simply matches nothing.
fn split_compound(command: &str) -> Vec<&str> {
    command
        .split(|c| matches!(c, ';' | '|' | '&' | '\n' | '(' | ')'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Iteratively drop env assignments (`FOO=bar`), wrapper commands, and
/// leftover wrapper flags so `sudo env FOO=1 nice -n 5 curl …` matches a
/// `curl` rule. Mirrors the destructive-table stripping in `tools.rs`.
fn strip_wrappers<'a>(toks: &[&'a str]) -> Vec<&'a str> {
    let mut tokens: &[&str] = toks;
    loop {
        match tokens.first() {
            Some(t) if is_env_assignment(t) => tokens = &tokens[1..],
            Some(&"sudo") | Some(&"env") | Some(&"command") | Some(&"builtin")
            | Some(&"nohup") | Some(&"time") | Some(&"nice") | Some(&"xargs")
            | Some(&"stdbuf") | Some(&"timeout") => tokens = &tokens[1..],
            Some(t) if t.starts_with('-') => tokens = &tokens[1..],
            // Bare integers are wrapper-flag values (`nice -n 5`,
            // `timeout 5`), never commands.
            Some(t) if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => {
                tokens = &tokens[1..]
            }
            _ => break,
        }
    }
    tokens.to_vec()
}

/// `VAR=value` prefix assignment.
fn is_env_assignment(t: &str) -> bool {
    t.split_once('=').is_some_and(|(k, _)| {
        !k.is_empty()
            && !k.starts_with(|c: char| c.is_ascii_digit())
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// `sh -c '<payload>'` (any `-…c…` short cluster): the payload rejoined
/// and quote-trimmed, or None when this is not a shell -c invocation.
fn shell_c_payload(stripped: &[&str]) -> Option<String> {
    let base = stripped.first()?.rsplit('/').next().unwrap_or(stripped[0]);
    if !matches!(base, "sh" | "bash" | "zsh" | "dash" | "ksh") {
        return None;
    }
    let rest = &stripped[1..];
    let at = rest
        .iter()
        .position(|t| t.starts_with('-') && !t.starts_with("--") && t.contains('c'))?;
    let payload = rest.get(at + 1..)?.join(" ");
    Some(payload.trim_matches(|c| c == '"' || c == '\'').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(deny: &str, ask: &str, allow: &str) -> Policy {
        let mut entries = Vec::new();
        for (k, v) in [("deny", deny), ("ask", ask), ("allow", allow)] {
            if !v.is_empty() {
                entries.push((k.to_string(), v.to_string()));
            }
        }
        Policy::from_entries(&entries)
    }

    fn deny_rule(p: &Policy, cmd: &str) -> Option<String> {
        match p.check(cmd) {
            Decision::Deny { rule } => Some(rule),
            _ => None,
        }
    }

    #[test]
    fn empty_policy_checks_nothing() {
        let p = Policy::default();
        assert!(p.is_empty());
        // Even command substitution passes: fail-closed applies only
        // once someone configured a policy.
        assert_eq!(p.check("echo $(hostname)"), Decision::Allowed);
    }

    #[test]
    fn parses_toml_arrays_and_comma_lists() {
        let a = policy(r#"["git push:*", "curl:*"]"#, "", "");
        let b = policy("git push:*, curl:*", "", "");
        for p in [a, b] {
            assert!(deny_rule(&p, "git push origin main").is_some());
            assert!(deny_rule(&p, "curl https://x.dev").is_some());
            assert_eq!(p.check("git pull"), Decision::Allowed);
        }
    }

    #[test]
    fn wildcard_vs_exact_rules() {
        let p = policy(r#"["npm publish"]"#, "", "");
        assert!(deny_rule(&p, "npm publish").is_some());
        // Exact rule: extra args do not match …
        assert_eq!(p.check("npm publish --dry-run"), Decision::Allowed);
        // … and a token prefix is not a word prefix.
        assert_eq!(p.check("npm publish-please"), Decision::Allowed);
    }

    #[test]
    fn compound_split_rejects_when_any_segment_denies() {
        let p = policy(r#"["curl:*"]"#, "", "");
        assert!(deny_rule(&p, "echo ok && curl https://x.dev").is_some());
        assert!(deny_rule(&p, "echo ok; curl https://x.dev").is_some());
        assert!(deny_rule(&p, "cat f | curl -T - https://x.dev").is_some());
        assert!(deny_rule(&p, "curl https://x.dev &").is_some());
        assert_eq!(p.check("echo ok && echo again"), Decision::Allowed);
    }

    #[test]
    fn wrapper_and_env_prefixes_are_stripped() {
        let p = policy(r#"["curl:*"]"#, "", "");
        for cmd in [
            "sudo curl https://x.dev",
            "env FOO=1 curl https://x.dev",
            "FOO=1 curl https://x.dev",
            "nice -n 5 curl https://x.dev",
            "timeout 5 curl https://x.dev",
            "/usr/bin/curl https://x.dev",
            "\\curl https://x.dev",
        ] {
            assert!(deny_rule(&p, cmd).is_some(), "not denied: {cmd}");
        }
    }

    #[test]
    fn shell_c_payload_is_checked() {
        let p = policy(r#"["curl:*"]"#, "", "");
        assert!(deny_rule(&p, "sh -c 'curl https://x.dev'").is_some());
        assert!(deny_rule(&p, "bash -lc \"curl https://x.dev\"").is_some());
        assert_eq!(p.check("sh -c 'echo ok'"), Decision::Allowed);
    }

    #[test]
    fn substitution_fails_closed_when_policy_configured() {
        let p = policy(r#"["curl:*"]"#, "", "");
        assert_eq!(
            p.check("echo `date`"),
            Decision::Deny { rule: "command substitution".into() }
        );
        assert_eq!(
            p.check("echo $(date)"),
            Decision::Deny { rule: "command substitution".into() }
        );
    }

    #[test]
    fn precedence_deny_over_ask_over_allow() {
        // A command matching deny + allow: deny wins.
        let p = policy(r#"["git push:*"]"#, "", r#"["git push:*"]"#);
        assert!(deny_rule(&p, "git push origin").is_some());
        // ask beats allow (deny > ask > allow, per the spec).
        let p = policy("", r#"["cargo publish:*"]"#, r#"["cargo publish:*"]"#);
        match p.check("cargo publish --dry-run") {
            Decision::Ask { rule } => assert_eq!(rule, "cargo publish:*"),
            other => panic!("expected Ask, got {other:?}"),
        }
        // Unmatched commands default to allowed.
        assert_eq!(p.check("cargo build"), Decision::Allowed);
    }

    #[test]
    fn repeated_keys_accumulate() {
        let entries = vec![
            ("deny".to_string(), "curl:*".to_string()),
            ("deny".to_string(), "wget:*".to_string()),
        ];
        let p = Policy::from_entries(&entries);
        assert!(deny_rule(&p, "curl x").is_some());
        assert!(deny_rule(&p, "wget x").is_some());
    }
}
