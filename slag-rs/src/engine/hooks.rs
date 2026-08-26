//! hooks — lifecycle hook engine.
//!
//! A hook is a side-effect the operator wires to a point in the forge's
//! life: before a tool runs, after it fails, when an ingot cracks. Config
//! lives in the `[hooks]` table of `slag.toml`, one hook per line, keyed
//! by event name.
//!
//! The protocol is the exit code, not the output:
//!
//! | exit | meaning |
//! |------|---------|
//! | 0 | stdout becomes model-visible context |
//! | 2 | **block**: the action is refused and stderr goes to the smith |
//! | other | logged and ignored — a broken hook never stops a forge |

use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Where in the forge's life a hook fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HookEvent {
    /// Before a tool dispatches. Exit 2 blocks the call.
    PreTool,
    /// After a tool returns successfully.
    PostTool,
    /// After a tool returns an error.
    ToolError,
    /// The smith finished its turn loop.
    Stop,
    /// A forge session opened.
    SessionStart,
    /// Before the agent compacts its context.
    PreCompact,
    /// An ingot passed its proof.
    IngotForged,
    /// An ingot burned its last heat.
    IngotCracked,
}

impl HookEvent {
    pub const ALL: [HookEvent; 8] = [
        HookEvent::PreTool,
        HookEvent::PostTool,
        HookEvent::ToolError,
        HookEvent::Stop,
        HookEvent::SessionStart,
        HookEvent::PreCompact,
        HookEvent::IngotForged,
        HookEvent::IngotCracked,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::PreTool => "pre_tool",
            HookEvent::PostTool => "post_tool",
            HookEvent::ToolError => "tool_error",
            HookEvent::Stop => "stop",
            HookEvent::SessionStart => "session_start",
            HookEvent::PreCompact => "pre_compact",
            HookEvent::IngotForged => "ingot_forged",
            HookEvent::IngotCracked => "ingot_cracked",
        }
    }

    /// Parse an event name. Dashes and camelCase spellings both land on
    /// the snake_case canonical form, so `preToolUse` and `pre-tool` are
    /// not silent no-ops in a config file.
    pub fn parse(s: &str) -> Option<HookEvent> {
        let norm: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        HookEvent::ALL
            .into_iter()
            .find(|e| e.as_str().replace('_', "") == norm || alias(*e) == norm)
    }
}

/// Claude Code's own event spellings, accepted verbatim so a config
/// copied from those docs works here.
fn alias(e: HookEvent) -> &'static str {
    match e {
        HookEvent::PreTool => "pretooluse",
        HookEvent::PostTool => "posttooluse",
        HookEvent::ToolError => "toolerror",
        HookEvent::Stop => "stop",
        HookEvent::SessionStart => "sessionstart",
        HookEvent::PreCompact => "precompact",
        HookEvent::IngotForged => "ingotforged",
        HookEvent::IngotCracked => "ingotcracked",
    }
}

/// What a hook actually does when it fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookKind {
    /// Shell command, run through `sh -c`. The default kind.
    Command(String),
    /// One LLM call through `judge.rs`, answering allow or block. The
    /// verifier for an ingot whose acceptance cannot be a shell command.
    Prompt { prompt: String, model: Option<String> },
    /// A one-ingot smith with tools. Refuses by emitting `BLOCK: reason`.
    Agent { prompt: String, model: Option<String> },
    /// POST the payload JSON to a URL. Headers interpolate only the env
    /// names listed in `allowed_env`; every other name resolves empty.
    Http {
        url: String,
        headers: Vec<(String, String)>,
        allowed_env: Vec<String>,
    },
}

/// Does this hook's matcher select `tool`? Three tiers, cheapest first:
///
/// 1. empty or `*` — every tool
/// 2. `^[a-zA-Z0-9_|]+$` — exact name, or `bash|edit_file` alternation
/// 3. anything else — regex, unanchored
///
/// An invalid regex logs once and selects nothing. A typo in a config
/// file costs you the hook, never the forge.
pub fn matches(matcher: &str, tool: &str) -> bool {
    let matcher = matcher.trim();
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    if matcher
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|')
    {
        return matcher.split('|').any(|alt| alt == tool);
    }
    match regex::Regex::new(matcher) {
        Ok(re) => re.is_match(tool),
        Err(e) => {
            warn_once(&format!("hook matcher `{matcher}` is not a valid regex ({e}); skipping"));
            false
        }
    }
}

/// One warning per distinct message per process. A bad matcher on a
/// pre_tool hook would otherwise print on every tool call of every ingot.
fn warn_once(msg: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut guard = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    let seen = guard.get_or_insert_with(HashSet::new);
    if seen.insert(msg.to_string()) {
        eprintln!("slag: {msg}");
    }
}

/// Evaluate an `if` precondition in-process, before any fork.
///
/// The form is `tool(glob)` — `bash(cargo *)` fires only for bash calls
/// whose command starts with `cargo`. A bare `glob` skips the tool check.
/// The glob runs against the raw argument string and against every string
/// value inside it, so `bash(cargo *)` sees `cargo build` rather than the
/// JSON wrapper around it.
///
/// With `MAX_ANVILS` smiths hammering in parallel, this gate is what
/// keeps a formatter hook from forking a process per unrelated tool call.
pub fn precondition_holds(cond: &str, tool: &str, arguments: &str) -> bool {
    let cond = cond.trim();
    if cond.is_empty() {
        return true;
    }
    let (want_tool, glob) = match cond.strip_suffix(')').and_then(|c| c.split_once('(')) {
        Some((t, g)) => (Some(t.trim()), g),
        None => (None, cond),
    };
    if let Some(want) = want_tool {
        if !matches(want, tool) {
            return false;
        }
    }
    if glob_match(glob, arguments) {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return false;
    };
    string_values(&value).iter().any(|v| glob_match(glob, v))
}

/// Every string leaf in a JSON value, in no particular order.
fn string_values(value: &Value) -> Vec<&str> {
    match value {
        Value::String(s) => vec![s.as_str()],
        Value::Array(items) => items.iter().flat_map(string_values).collect(),
        Value::Object(map) => map.values().flat_map(string_values).collect(),
        _ => Vec::new(),
    }
}

/// Shell-style glob: `*` spans any run (including empty), `?` is one
/// character, everything else is a literal. Backtracking with a restart
/// point, so it stays linear on the patterns people actually write.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut restart) = (None, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            restart = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            restart += 1;
            ti = restart;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// One configured hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDef {
    pub name: String,
    pub event: HookEvent,
    /// Tool-name matcher (item 70). `*` or empty matches every tool.
    pub matcher: String,
    pub kind: HookKind,
    pub timeout: Duration,
    pub if_cond: Option<String>,
    /// Fire at most once per process, then drop out of the session list.
    pub once: bool,
    /// Background the hook: the smith does not wait for it.
    pub run_async: bool,
    /// An async hook that exits 2 pushes its reason onto the smith's
    /// steer queue instead of blocking a call that already ran.
    pub async_rewake: bool,
}

/// Hooks that never answer are hangs, not hooks.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

impl HookDef {
    /// Parse one `[hooks]` line: an event key and a field string of
    /// `key=value` tokens split with shell quoting, so a command with
    /// spaces survives (`cmd='cargo fmt --all'`).
    ///
    /// Returns `None` — never panics, never aborts the load — for an
    /// unknown event, unparseable fields, or a hook with nothing to run.
    /// A malformed line in a config file must not take a forge down.
    pub fn parse(event_key: &str, spec: &str, index: usize) -> Option<HookDef> {
        let event = HookEvent::parse(event_key)?;
        let tokens = shell_words::split(spec).ok()?;

        let mut name = None;
        let mut matcher = String::new();
        let mut cmd = None;
        let mut prompt = None;
        let mut agent = None;
        let mut url = None;
        let mut model = None;
        let mut headers: Vec<(String, String)> = Vec::new();
        let mut allowed_env: Vec<String> = Vec::new();
        let mut timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
        let mut if_cond = None;
        let mut once = false;
        let mut run_async = false;
        let mut async_rewake = false;

        for token in tokens {
            let Some((key, value)) = token.split_once('=') else {
                continue;
            };
            match normalize_key(key).as_str() {
                "name" => name = Some(value.to_string()),
                "matcher" | "match" | "tool" => matcher = value.to_string(),
                "cmd" | "command" => cmd = Some(value.to_string()),
                "prompt" => prompt = Some(value.to_string()),
                "agent" => agent = Some(value.to_string()),
                "url" => url = Some(value.to_string()),
                "model" => model = Some(value.to_string()),
                "header" => {
                    if let Some((h, v)) = value.split_once(':') {
                        headers.push((h.trim().to_string(), v.trim().to_string()));
                    }
                }
                "allowedenvvars" | "allowedenv" => {
                    allowed_env.extend(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    );
                }
                "timeout" => {
                    if let Ok(secs) = value.parse::<u64>() {
                        timeout = Duration::from_secs(secs);
                    }
                }
                "if" => if_cond = Some(value.to_string()),
                "once" => once = crate::config::truthy(value),
                "async" => run_async = crate::config::truthy(value),
                "asyncrewake" => {
                    async_rewake = crate::config::truthy(value);
                    // Rewaking is what an async hook does with its exit 2;
                    // asking for one implies the other.
                    run_async = run_async || async_rewake;
                }
                _ => {}
            }
        }

        // Exactly one kind per hook. Two is a confused config line, and
        // guessing which the operator meant is worse than dropping it:
        // a hook that silently runs the wrong thing is the one failure
        // mode a verifier cannot afford.
        let declared = [
            cmd.is_some(),
            prompt.is_some(),
            agent.is_some(),
            url.is_some(),
        ]
        .iter()
        .filter(|d| **d)
        .count();
        if declared > 1 {
            warn_once(&format!(
                "hook on `{event_key}` declares more than one of cmd/prompt/agent/url; skipping"
            ));
            return None;
        }
        let kind = match (cmd, prompt, agent, url) {
            (Some(cmd), ..) => HookKind::Command(cmd),
            (_, Some(prompt), ..) => HookKind::Prompt { prompt, model },
            (_, _, Some(prompt), _) => HookKind::Agent { prompt, model },
            (.., Some(url)) => HookKind::Http {
                url,
                headers,
                allowed_env,
            },
            _ => return None,
        };
        Some(HookDef {
            name: name.unwrap_or_else(|| format!("{}#{index}", event.as_str())),
            event,
            matcher,
            kind,
            timeout,
            if_cond,
            once,
            run_async,
            async_rewake,
        })
    }
}

/// `allowedEnvVars` and `allowed_env_vars` are the same key.
fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// What one hook run produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookOutcome {
    pub name: String,
    pub code: i32,
    /// Exit 0 stdout: becomes model-visible context.
    pub context: Option<String>,
    /// Exit 2 stderr: the action is refused and this reaches the smith.
    pub block: Option<String>,
    /// PreToolUse rewrite: replaces the tool's arguments before it runs.
    pub updated_input: Option<Value>,
    /// Appended to the tool result the smith reads.
    pub additional_context: Option<String>,
    pub duration_ms: u128,
}

impl HookOutcome {
    pub fn blocked(&self) -> bool {
        self.block.is_some()
    }
}

/// Read a hook's structured reply, if it wrote one.
///
/// A hook that prints a JSON object carrying `updated_input` or
/// `additional_context` is talking to the engine; anything else is plain
/// output bound for the model. Both keys are optional and a hook may
/// send either alone — `{"additional_context": "3 clippy warnings"}`
/// injects lint output without a model round-trip, and
/// `{"updated_input": {"command": "ls"}}` rewrites a dangerous call in
/// place. Underscored and camelCase spellings both work.
fn structured(stdout: &str) -> Option<(Option<Value>, Option<String>)> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let obj = value.as_object()?;
    let pick = |a: &str, b: &str| obj.get(a).or_else(|| obj.get(b));

    let updated = pick("updated_input", "updatedInput").cloned();
    let context = pick("additional_context", "additionalContext").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    });
    if updated.is_none() && context.is_none() {
        return None;
    }
    Some((updated, context))
}

/// Apply the exit-code protocol to one finished hook process.
///
/// Exit 0 promotes stdout to context; exit 2 blocks with stderr (falling
/// back to stdout when the hook wrote its reason there instead); any
/// other code is a hook bug, logged by the caller and otherwise ignored.
pub fn classify(name: &str, code: i32, stdout: &str, stderr: &str, duration_ms: u128) -> HookOutcome {
    let mut outcome = HookOutcome {
        name: name.to_string(),
        code,
        duration_ms,
        ..Default::default()
    };
    match code {
        0 => {
            let out = stdout.trim();
            if let Some((updated, extra)) = structured(out) {
                outcome.updated_input = updated;
                outcome.additional_context = extra;
            } else if !out.is_empty() {
                outcome.context = Some(out.to_string());
            }
        }
        2 => {
            let reason = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            outcome.block = Some(if reason.is_empty() {
                format!("hook `{name}` blocked this action")
            } else {
                reason.to_string()
            });
        }
        _ => {}
    }
    outcome
}

/// The JSON a hook reads on stdin. Fields absent from an event's
/// context are omitted rather than nulled, so `jq -e .tool_name` is a
/// usable guard inside a hook script.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HookPayload {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Parsed tool arguments, when the call carried valid JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl HookPayload {
    pub fn new(event: HookEvent) -> Self {
        Self {
            event: event.as_str().to_string(),
            cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
            ..Default::default()
        }
    }

    pub fn with_tool(mut self, name: &str, arguments: &str) -> Self {
        self.tool_name = Some(name.to_string());
        self.tool_input = serde_json::from_str(arguments).ok();
        self
    }

    pub fn with_output(mut self, output: &str) -> Self {
        self.tool_output = Some(output.to_string());
        self
    }

    pub fn with_ingot(mut self, id: &str) -> Self {
        self.ingot_id = Some(id.to_string());
        self
    }

    fn to_stdin(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Timed-out and un-spawnable hooks report this code. Negative so it can
/// never collide with a real exit status, and outside {0, 2} so the
/// protocol treats it as "logged and ignored": a hook that hangs or is
/// missing must not wedge a forge, and must not block a tool either.
pub const CODE_FAILED: i32 = -1;

/// Run one command hook to completion under its timeout, feeding the
/// payload on stdin and applying the exit-code protocol to the result.
pub async fn run_command(def: &HookDef, payload: &HookPayload) -> HookOutcome {
    let started = Instant::now();
    let HookKind::Command(cmd) = &def.kind else {
        return failed(def, started, format!("hook `{}` is not a command", def.name));
    };

    let spawned = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return failed(def, started, format!("hook `{}` failed to spawn: {e}", def.name)),
    };

    if let Some(mut stdin) = child.stdin.take() {
        // A hook that ignores stdin closes the pipe early; that EPIPE is
        // the hook's business, not an error worth failing the run over.
        let _ = stdin.write_all(payload.to_stdin().as_bytes()).await;
        drop(stdin);
    }

    match tokio::time::timeout(def.timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => classify(
            &def.name,
            out.status.code().unwrap_or(CODE_FAILED),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
            started.elapsed().as_millis(),
        ),
        Ok(Err(e)) => failed(def, started, format!("hook `{}` failed: {e}", def.name)),
        Err(_) => failed(
            def,
            started,
            format!("hook `{}` timed out after {}s", def.name, def.timeout.as_secs()),
        ),
    }
}

/// Dispatch one hook on its kind. Every kind funnels into the same
/// `HookOutcome`, so the exit-code protocol of item 69 stays the single
/// vocabulary the caller has to understand.
pub async fn run_hook(snap: &HookSnapshot, def: &HookDef, payload: &HookPayload) -> HookOutcome {
    let started = Instant::now();
    match &def.kind {
        HookKind::Command(_) => run_command(def, payload).await,
        HookKind::Prompt { prompt, model } => {
            let Some(cfg) = &snap.engine else {
                return failed(def, started, no_config(&def.name));
            };
            let provider = crate::engine::provider::OpenRouter::with_base_url(
                cfg.api_key.clone(),
                cfg.base_url.clone(),
            );
            let model = model.clone().unwrap_or_else(|| cfg.model_judge.clone());
            run_prompt(def, payload, &provider, &model, prompt).await
        }
        HookKind::Agent { prompt, model } => {
            let Some(cfg) = &snap.engine else {
                return failed(def, started, no_config(&def.name));
            };
            let mut cfg = cfg.clone();
            if let Some(m) = model {
                cfg.model_base = m.clone();
            }
            // Grade 1: the base model, the cheapest path. A verifier that
            // costs more than the work it guards does not get used.
            let smith = crate::smith::make_smith(&cfg, "default", 1, &Default::default());
            run_agent(def, payload, smith.as_ref(), prompt).await
        }
        HookKind::Http {
            url,
            headers,
            allowed_env,
        } => run_http(def, payload, url, headers, allowed_env).await,
    }
}

fn no_config(name: &str) -> String {
    format!("hook `{name}` needs an OpenRouter config and none is loaded")
}

/// One LLM call answering allow or block, through `judge.rs::rule`.
/// Injectable provider, so the decision logic tests without a network.
pub async fn run_prompt(
    def: &HookDef,
    payload: &HookPayload,
    provider: &dyn crate::engine::Provider,
    model: &str,
    instruction: &str,
) -> HookOutcome {
    let started = Instant::now();
    let body = payload.to_stdin();
    let ruled = tokio::time::timeout(
        def.timeout,
        crate::engine::tools::judge::rule(provider, model, instruction, &body),
    )
    .await;
    match ruled {
        Ok(Ok(r)) if r.block => HookOutcome {
            name: def.name.clone(),
            code: 2,
            block: Some(r.reason),
            duration_ms: started.elapsed().as_millis(),
            ..Default::default()
        },
        Ok(Ok(r)) => HookOutcome {
            name: def.name.clone(),
            code: 0,
            context: (!r.reason.trim().is_empty()).then_some(r.reason),
            duration_ms: started.elapsed().as_millis(),
            ..Default::default()
        },
        Ok(Err(e)) => failed(def, started, format!("hook `{}` failed: {e}", def.name)),
        Err(_) => failed(
            def,
            started,
            format!("hook `{}` timed out after {}s", def.name, def.timeout.as_secs()),
        ),
    }
}

/// A one-ingot smith with tools. It refuses by opening a line with
/// `BLOCK:`; anything else it says becomes context.
pub async fn run_agent(
    def: &HookDef,
    payload: &HookPayload,
    smith: &dyn crate::smith::Smith,
    instruction: &str,
) -> HookOutcome {
    let started = Instant::now();
    let prompt = format!("{instruction}\n\nEvent payload (JSON):\n{}", payload.to_stdin());
    match tokio::time::timeout(def.timeout, smith.invoke(&prompt)).await {
        Ok(Ok(out)) => match block_line(&out) {
            Some(reason) => HookOutcome {
                name: def.name.clone(),
                code: 2,
                block: Some(reason),
                duration_ms: started.elapsed().as_millis(),
                ..Default::default()
            },
            None => classify(
                &def.name,
                0,
                &out,
                "",
                started.elapsed().as_millis(),
            ),
        },
        Ok(Err(e)) => failed(def, started, format!("hook `{}` failed: {e}", def.name)),
        Err(_) => failed(
            def,
            started,
            format!("hook `{}` timed out after {}s", def.name, def.timeout.as_secs()),
        ),
    }
}

/// The first `BLOCK:` line's reason, if the agent refused.
fn block_line(out: &str) -> Option<String> {
    out.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("BLOCK:"))
        .map(|r| r.trim().to_string())
}

/// POST the payload JSON to a webhook.
pub async fn run_http(
    def: &HookDef,
    payload: &HookPayload,
    url: &str,
    headers: &[(String, String)],
    allowed_env: &[String],
) -> HookOutcome {
    let started = Instant::now();
    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .header("content-type", "application/json")
        .body(payload.to_stdin());
    for (name, value) in headers {
        req = req.header(name, interpolate(value, allowed_env));
    }

    match tokio::time::timeout(def.timeout, req.send()).await {
        Ok(Ok(resp)) => {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            http_outcome(&def.name, status, &body, started.elapsed().as_millis())
        }
        Ok(Err(e)) => failed(def, started, format!("hook `{}` failed: {e}", def.name)),
        Err(_) => failed(
            def,
            started,
            format!("hook `{}` timed out after {}s", def.name, def.timeout.as_secs()),
        ),
    }
}

/// Map an HTTP status onto the exit-code protocol, reusing HTTP's own
/// vocabulary rather than inventing a second one: 2xx accepts, 403
/// refuses, everything else is a webhook problem and is logged. A
/// notification endpoint that is down must never wedge a forge.
pub fn http_outcome(name: &str, status: u16, body: &str, duration_ms: u128) -> HookOutcome {
    match status {
        200..=299 => classify(name, 0, body, "", duration_ms),
        403 => HookOutcome {
            name: name.to_string(),
            code: 2,
            block: Some(if body.trim().is_empty() {
                "webhook refused the action".to_string()
            } else {
                body.trim().to_string()
            }),
            duration_ms,
            ..Default::default()
        },
        _ => HookOutcome {
            name: name.to_string(),
            code: CODE_FAILED,
            context: Some(format!("hook `{name}` got HTTP {status}")),
            duration_ms,
            ..Default::default()
        },
    }
}

/// Expand `$NAME` and `${NAME}` in a header value, but only for names in
/// `allowed`. Everything else resolves to the empty string, so a config
/// line cannot exfiltrate an unlisted variable by naming it.
///
/// Built by scan rather than regex replacement: the same discipline the
/// recipe span expander follows. A replace-into-a-template pass lets a
/// value that itself contains `$OTHER` get expanded on a second lap.
pub fn interpolate(value: &str, allowed: &[String]) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // `$$` is a literal dollar; a trailing `$` is one too.
        if i + 1 >= chars.len() {
            out.push('$');
            break;
        }
        if chars[i + 1] == '$' {
            out.push('$');
            i += 2;
            continue;
        }
        let (name, next) = if chars[i + 1] == '{' {
            match chars[i + 2..].iter().position(|c| *c == '}') {
                Some(off) => (
                    chars[i + 2..i + 2 + off].iter().collect::<String>(),
                    i + 3 + off,
                ),
                None => {
                    out.push('$');
                    i += 1;
                    continue;
                }
            }
        } else {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            (chars[i + 1..j].iter().collect::<String>(), j)
        };
        if name.is_empty() {
            out.push('$');
            i += 1;
            continue;
        }
        if allowed.iter().any(|a| a == &name) {
            out.push_str(&std::env::var(&name).unwrap_or_default());
        }
        i = next;
    }
    out
}

/// A hook that never produced an exit status. Carries its reason as
/// `context` so the failure shows up in logs, but neither blocks nor
/// pretends to have succeeded.
fn failed(def: &HookDef, started: Instant, reason: String) -> HookOutcome {
    HookOutcome {
        name: def.name.clone(),
        code: CODE_FAILED,
        context: Some(reason),
        duration_ms: started.elapsed().as_millis(),
        ..Default::default()
    }
}

/// What one event gives a hook and what it does with the answer. The
/// contract lives here, next to the code that honours it, and `slag
/// hooks list` prints it — documentation that cannot drift from the
/// implementation without a compile error.
pub struct EventDoc {
    pub event: HookEvent,
    /// When it fires.
    pub summary: &'static str,
    /// Payload fields beyond `event` and `cwd`.
    pub stdin: &'static str,
    /// What exit 2 refuses. Empty means nothing to refuse.
    pub blocks: &'static str,
}

pub const EVENTS: [EventDoc; 8] = [
    EventDoc {
        event: HookEvent::PreTool,
        summary: "before a tool dispatches",
        stdin: "tool_name, tool_input",
        blocks: "the tool call; stderr becomes the tool result",
    },
    EventDoc {
        event: HookEvent::PostTool,
        summary: "after a tool succeeds",
        stdin: "tool_name, tool_input, tool_output",
        blocks: "nothing; the reason is appended to the result",
    },
    EventDoc {
        event: HookEvent::ToolError,
        summary: "after a tool fails",
        stdin: "tool_name, tool_input, tool_output",
        blocks: "nothing; the reason is appended to the result",
    },
    EventDoc {
        event: HookEvent::Stop,
        summary: "the smith finished its turn loop",
        stdin: "—",
        blocks: "nothing",
    },
    EventDoc {
        event: HookEvent::SessionStart,
        summary: "a forge session opened",
        stdin: "—",
        blocks: "nothing",
    },
    EventDoc {
        event: HookEvent::PreCompact,
        summary: "before the agent compacts its context",
        stdin: "—",
        blocks: "nothing",
    },
    EventDoc {
        event: HookEvent::IngotForged,
        summary: "an ingot passed its proof",
        stdin: "ingot_id",
        blocks: "nothing",
    },
    EventDoc {
        event: HookEvent::IngotCracked,
        summary: "an ingot burned its last heat",
        stdin: "ingot_id",
        blocks: "nothing",
    },
];

/// `slag hooks list`: the event contract, then the hooks configured
/// against it. Plain text — this is reference output, read once.
pub fn describe(snap: &HookSnapshot) -> String {
    let mut out = String::from("EVENTS\n");
    for doc in &EVENTS {
        out.push_str(&format!(
            "  {:<14} {}\n                 stdin: {}\n                 exit 2: {}\n",
            doc.event.as_str(),
            doc.summary,
            doc.stdin,
            doc.blocks
        ));
    }
    out.push_str("\nEXIT CODES\n");
    out.push_str("  0   stdout becomes model-visible context\n");
    out.push_str("      (or JSON with updated_input / additional_context)\n");
    out.push_str("  2   block: stderr goes to the smith\n");
    out.push_str("  *   logged and ignored\n");

    out.push_str("\nCONFIGURED\n");
    if snap.disabled {
        out.push_str("  disable_all_hooks is set: nothing will fire.\n");
    }
    if snap.hooks.is_empty() {
        out.push_str("  none. Add a [hooks] table to slag.toml:\n");
        out.push_str("    [hooks]\n");
        out.push_str("    post_tool = \"name=fmt matcher=edit_file cmd='cargo fmt'\"\n");
        return out;
    }
    for h in &snap.hooks {
        let cmd = match &h.kind {
            HookKind::Command(cmd) => format!("cmd: {cmd}"),
            HookKind::Prompt { prompt, model } => {
                format!("prompt: {prompt}{}", model_suffix(model))
            }
            HookKind::Agent { prompt, model } => {
                format!("agent: {prompt}{}", model_suffix(model))
            }
            HookKind::Http {
                url, allowed_env, ..
            } => format!(
                "url: {url}{}",
                if allowed_env.is_empty() {
                    String::new()
                } else {
                    format!("  (env: {})", allowed_env.join(", "))
                }
            ),
        };
        let mut flags = vec![format!("timeout={}s", h.timeout.as_secs())];
        if h.once {
            flags.push("once".into());
        }
        if h.async_rewake {
            flags.push("asyncRewake".into());
        } else if h.run_async {
            flags.push("async".into());
        }
        if let Some(cond) = &h.if_cond {
            flags.push(format!("if={cond}"));
        }
        out.push_str(&format!(
            "  {:<14} {} [{}]\n                 matcher: {}\n                 {cmd}\n",
            h.event.as_str(),
            h.name,
            flags.join(", "),
            if h.matcher.is_empty() { "*" } else { &h.matcher },
        ));
    }
    out
}

fn model_suffix(model: &Option<String>) -> String {
    model
        .as_deref()
        .map(|m| format!("  (model: {m})"))
        .unwrap_or_default()
}

/// Announce a hook run so a slow hook reads as work, not a hang.
fn announce_start(tx: &Option<crate::engine::EventTx>, def: &HookDef) {
    crate::engine::emit(
        tx,
        crate::engine::EngineEvent::HookStarted {
            name: def.name.clone(),
            hook_event: def.event.as_str().to_string(),
            status_message: Some(format!("running hook `{}`", def.name)),
        },
    );
}

fn announce_finish(tx: &Option<crate::engine::EventTx>, def: &HookDef, outcome: &HookOutcome) {
    crate::engine::emit(
        tx,
        crate::engine::EngineEvent::HookFinished {
            name: def.name.clone(),
            hook_event: def.event.as_str().to_string(),
            code: outcome.code,
            duration_ms: outcome.duration_ms as u64,
        },
    );
}

/// The hook config as it stood when the forge opened.
///
/// Read once, then frozen. `forge.rs` hands the workspace to smiths that
/// can rewrite any file in it, `slag.toml` included — without this
/// freeze, a smith could register a hook into the session that is
/// currently running it.
#[derive(Default)]
pub struct HookSnapshot {
    pub hooks: Vec<HookDef>,
    /// `disable_all_hooks` in config, or `SLAG_DISABLE_HOOKS` in env.
    pub disabled: bool,
    /// Models and key for the Prompt and Agent kinds, frozen with the
    /// hooks: a smith rewriting `slag.toml` mid-run cannot repoint its
    /// own verifier at a cheaper model.
    pub engine: Option<crate::config::EngineConfig>,
}

/// Hand-written so the API key can never reach a log line. `EngineConfig`
/// holds it, and a derived `Debug` on the snapshot would print it.
impl std::fmt::Debug for HookSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookSnapshot")
            .field("hooks", &self.hooks)
            .field("disabled", &self.disabled)
            .field("engine", &self.engine.is_some())
            .finish()
    }
}

impl HookSnapshot {
    /// Parse a `[hooks]` table's entries, dropping lines that do not
    /// describe a runnable hook.
    pub fn from_entries(entries: Vec<(String, String)>, disabled: bool) -> Self {
        let hooks = entries
            .into_iter()
            .enumerate()
            .filter_map(|(i, (event, spec))| HookDef::parse(&event, &spec, i))
            .collect();
        Self {
            hooks,
            disabled,
            engine: None,
        }
    }

    /// Freeze the model config alongside the hooks.
    pub fn with_engine(mut self, engine: Option<crate::config::EngineConfig>) -> Self {
        self.engine = engine;
        self
    }

    /// Hooks bound to `event` whose matcher selects `tool`, in config
    /// order. Empty whenever the kill switch is on.
    pub fn select(&self, event: HookEvent, tool: &str) -> Vec<&HookDef> {
        if self.disabled {
            return Vec::new();
        }
        self.hooks
            .iter()
            .filter(|h| h.event == event && matches(&h.matcher, tool))
            .collect()
    }
}

static SNAPSHOT: std::sync::OnceLock<std::sync::Arc<HookSnapshot>> = std::sync::OnceLock::new();

/// The process-wide snapshot, loaded from config on first call.
pub fn snapshot() -> std::sync::Arc<HookSnapshot> {
    SNAPSHOT
        .get_or_init(|| {
            std::sync::Arc::new(
                HookSnapshot::from_entries(
                    crate::config::hook_entries(),
                    crate::config::disable_all_hooks(),
                )
                .with_engine(crate::config::EngineConfig::load()),
            )
        })
        .clone()
}

/// Install a snapshot before anything reads one. Returns false when a
/// snapshot already exists — the freeze holds even against this.
pub fn install_snapshot(snap: HookSnapshot) -> bool {
    SNAPSHOT.set(std::sync::Arc::new(snap)).is_ok()
}

/// `once` bookkeeping: names that have already fired this process.
fn spent() -> std::sync::MutexGuard<'static, std::collections::HashSet<String>> {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SPENT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SPENT
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Claim a `once` hook's single run. False means it already fired.
fn claim_once(def: &HookDef) -> bool {
    !def.once || spent().insert(def.name.clone())
}

/// What a whole event's worth of hooks decided.
#[derive(Debug, Clone, Default)]
pub struct HookRun {
    /// Set by the first hook to exit 2. The action is refused.
    pub blocked: Option<String>,
    /// Exit-0 stdout from every hook that produced some, in order, plus
    /// every `additional_context` a structured hook asked to inject.
    pub context: Vec<String>,
    /// The last `updated_input` any hook returned. Later hooks see the
    /// call as configured, not as rewritten — a chain of rewriters is a
    /// config smell, and last-writer-wins is the rule that stays legible
    /// when someone writes one anyway.
    pub updated_input: Option<Value>,
    pub outcomes: Vec<HookOutcome>,
}

impl HookRun {
    pub fn blocked(&self) -> bool {
        self.blocked.is_some()
    }

    /// The context lines as one block, or `None` when no hook spoke.
    pub fn context_block(&self) -> Option<String> {
        if self.context.is_empty() {
            None
        } else {
            Some(self.context.join("\n"))
        }
    }

    /// The rewritten argument JSON to dispatch with, when a PreToolUse
    /// hook asked for one. Objects only: a hook returning a bare string
    /// or number is confused, and swapping it in would break the tool.
    pub fn rewritten_arguments(&self) -> Option<String> {
        self.updated_input
            .as_ref()
            .filter(|v| v.is_object())
            .map(|v| v.to_string())
    }
}

/// Fire every hook bound to `event`.
///
/// `tool` and `arguments` drive the matcher (item 70) and the in-process
/// `if` gate (item 73); pass `""` for events with no tool in scope.
///
/// Synchronous hooks run in config order and the first exit 2 stops the
/// rest — a blocked action has nothing left to decide. Async hooks are
/// backgrounded; an `asyncRewake` one that exits 2 pushes its reason onto
/// `steer`, reaching the smith at its next turn rather than blocking a
/// call that already ran.
pub async fn fire(
    event: HookEvent,
    payload: HookPayload,
    tool: &str,
    arguments: &str,
    steer: Option<&crate::engine::SteerQueue>,
    events: Option<&crate::engine::EventTx>,
) -> HookRun {
    fire_with(&snapshot(), event, payload, tool, arguments, steer, events).await
}

/// `fire` against an explicit snapshot instead of the process-wide one.
pub async fn fire_with(
    snap: &HookSnapshot,
    event: HookEvent,
    payload: HookPayload,
    tool: &str,
    arguments: &str,
    steer: Option<&crate::engine::SteerQueue>,
    events: Option<&crate::engine::EventTx>,
) -> HookRun {
    let mut run = HookRun::default();
    let tx = events.cloned();

    for def in snap.select(event, tool) {
        if let Some(cond) = &def.if_cond {
            if !precondition_holds(cond, tool, arguments) {
                continue;
            }
        }
        if !claim_once(def) {
            continue;
        }

        // Only command hooks background: the other kinds borrow the
        // frozen snapshot (models, key) and cannot outlive it in a
        // spawned task. They run inline, under their own timeout.
        if def.run_async && matches!(def.kind, HookKind::Command(_)) {
            let def = def.clone();
            let payload = payload.clone();
            let steer = steer.cloned();
            let tx = tx.clone();
            tokio::spawn(async move {
                announce_start(&tx, &def);
                let outcome = run_command(&def, &payload).await;
                announce_finish(&tx, &def, &outcome);
                if def.async_rewake {
                    if let (Some(reason), Some(queue)) = (outcome.block, steer) {
                        if let Ok(mut q) = queue.lock() {
                            q.push(format!("hook `{}`: {reason}", def.name));
                        }
                    }
                }
            });
            continue;
        }

        announce_start(&tx, def);
        let outcome = run_hook(snap, def, &payload).await;
        announce_finish(&tx, def, &outcome);
        if let Some(ctx) = &outcome.context {
            run.context.push(ctx.clone());
        }
        if let Some(extra) = &outcome.additional_context {
            run.context.push(extra.clone());
        }
        if outcome.updated_input.is_some() {
            run.updated_input = outcome.updated_input.clone();
        }
        let blocked = outcome.block.clone();
        run.outcomes.push(outcome);
        if let Some(reason) = blocked {
            run.blocked = Some(reason);
            break;
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_hook(cmd: &str) -> HookDef {
        HookDef {
            name: "t".into(),
            event: HookEvent::PreTool,
            matcher: String::new(),
            kind: HookKind::Command(cmd.into()),
            timeout: Duration::from_secs(5),
            if_cond: None,
            once: false,
            run_async: false,
            async_rewake: false,
        }
    }

    #[test]
    fn event_names_round_trip_and_accept_claude_spellings() {
        for e in HookEvent::ALL {
            assert_eq!(HookEvent::parse(e.as_str()), Some(e));
        }
        assert_eq!(HookEvent::parse("PreToolUse"), Some(HookEvent::PreTool));
        assert_eq!(HookEvent::parse("pre-tool"), Some(HookEvent::PreTool));
        assert_eq!(HookEvent::parse("PostToolUse"), Some(HookEvent::PostTool));
        assert_eq!(HookEvent::parse("ingot-cracked"), Some(HookEvent::IngotCracked));
        assert_eq!(HookEvent::parse("nonsense"), None);
    }

    #[test]
    fn parse_reads_fields_with_shell_quoting() {
        let h = HookDef::parse("post_tool", "name=fmt matcher=edit_file cmd='cargo fmt --all' timeout=10", 0)
            .expect("parses");
        assert_eq!(h.name, "fmt");
        assert_eq!(h.event, HookEvent::PostTool);
        assert_eq!(h.matcher, "edit_file");
        assert_eq!(h.kind, HookKind::Command("cargo fmt --all".into()));
        assert_eq!(h.timeout, Duration::from_secs(10));
    }

    #[test]
    fn parse_defaults_name_and_timeout() {
        let h = HookDef::parse("pre_tool", "cmd=./guard.sh", 3).expect("parses");
        assert_eq!(h.name, "pre_tool#3");
        assert_eq!(h.matcher, "");
        assert_eq!(h.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert!(h.if_cond.is_none());
    }

    #[test]
    fn malformed_lines_skip_instead_of_panicking() {
        // Unknown event.
        assert!(HookDef::parse("whenever", "cmd=x", 0).is_none());
        // Nothing to run.
        assert!(HookDef::parse("stop", "matcher=* timeout=5", 0).is_none());
        // Unbalanced quote: shell-words refuses, we skip.
        assert!(HookDef::parse("stop", "cmd='unterminated", 0).is_none());
        // A junk token between good ones is ignored, not fatal.
        let h = HookDef::parse("stop", "junk cmd=echo timeout=notanumber", 0).expect("parses");
        assert_eq!(h.kind, HookKind::Command("echo".into()));
        assert_eq!(h.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    #[test]
    fn exit_zero_promotes_stdout_to_context() {
        let o = classify("lint", 0, "  3 warnings  ", "noise", 12);
        assert_eq!(o.context.as_deref(), Some("3 warnings"));
        assert!(o.block.is_none());
        assert!(!o.blocked());
        assert_eq!(o.duration_ms, 12);
    }

    #[test]
    fn exit_zero_with_empty_stdout_adds_nothing() {
        let o = classify("lint", 0, "   \n", "", 1);
        assert!(o.context.is_none());
    }

    #[test]
    fn exit_two_blocks_and_carries_stderr_to_the_smith() {
        let o = classify("guard", 2, "", "rm -rf is not allowed here", 5);
        assert!(o.blocked());
        assert_eq!(o.block.as_deref(), Some("rm -rf is not allowed here"));
        assert!(o.context.is_none());
    }

    #[test]
    fn exit_two_falls_back_to_stdout_then_to_a_default_reason() {
        let o = classify("guard", 2, "wrote the reason to stdout", "", 5);
        assert_eq!(o.block.as_deref(), Some("wrote the reason to stdout"));

        let silent = classify("guard", 2, "", "", 5);
        assert_eq!(silent.block.as_deref(), Some("hook `guard` blocked this action"));
    }

    #[test]
    fn other_exit_codes_neither_block_nor_inject() {
        let o = classify("flaky", 127, "some output", "command not found", 3);
        assert!(!o.blocked());
        assert!(o.context.is_none());
        assert_eq!(o.code, 127);
    }

    #[tokio::test]
    async fn a_command_hook_reads_the_payload_on_stdin() {
        let payload = HookPayload::new(HookEvent::PreTool)
            .with_tool("bash", r#"{"command":"cargo build"}"#);
        let o = run_command(&cmd_hook("cat"), &payload).await;
        assert_eq!(o.code, 0);
        let ctx = o.context.expect("stdout became context");
        assert!(ctx.contains(r#""event":"pre_tool""#), "{ctx}");
        assert!(ctx.contains(r#""tool_name":"bash""#), "{ctx}");
        assert!(ctx.contains(r#""command":"cargo build""#), "{ctx}");
    }

    #[tokio::test]
    async fn unparseable_tool_arguments_omit_tool_input_without_failing() {
        let payload = HookPayload::new(HookEvent::PreTool).with_tool("bash", "not json");
        assert_eq!(payload.tool_name.as_deref(), Some("bash"));
        assert!(payload.tool_input.is_none());
        assert!(!payload.to_stdin().contains("tool_input"));
    }

    #[tokio::test]
    async fn exit_two_from_a_real_process_blocks() {
        let hook = cmd_hook("echo 'no rm here' >&2; exit 2");
        let o = run_command(&hook, &HookPayload::new(HookEvent::PreTool)).await;
        assert_eq!(o.code, 2);
        assert_eq!(o.block.as_deref(), Some("no rm here"));
    }

    #[tokio::test]
    async fn a_hook_that_overruns_its_timeout_neither_blocks_nor_hangs() {
        let mut hook = cmd_hook("sleep 30");
        hook.timeout = Duration::from_millis(120);
        let o = tokio::time::timeout(Duration::from_secs(5), run_command(&hook, &HookPayload::new(HookEvent::Stop)))
            .await
            .expect("run_command returns well before the sleep would");
        assert_eq!(o.code, CODE_FAILED);
        assert!(!o.blocked(), "a slow hook must never block the smith");
        assert!(o.context.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn a_hook_that_cannot_run_is_logged_not_fatal() {
        let o = run_command(&cmd_hook("slag-no-such-binary-xyz"), &HookPayload::new(HookEvent::Stop)).await;
        assert_ne!(o.code, 0);
        assert!(!o.blocked());
    }

    // ---- item 70: the three-tier matcher ----

    #[test]
    fn wildcard_and_empty_matchers_select_every_tool() {
        for m in ["", "*", "  "] {
            assert!(matches(m, "bash"));
            assert!(matches(m, "edit_file"));
        }
    }

    #[test]
    fn plain_names_match_exactly_not_as_substrings() {
        assert!(matches("bash", "bash"));
        assert!(!matches("bash", "bash_other"));
        assert!(!matches("edit", "edit_file"));
    }

    #[test]
    fn pipe_alternation_takes_the_fast_path() {
        assert!(matches("bash|edit_file", "bash"));
        assert!(matches("bash|edit_file", "edit_file"));
        assert!(!matches("bash|edit_file", "read_file"));
    }

    #[test]
    fn anything_outside_the_fast_path_charset_falls_back_to_regex() {
        assert!(matches("^edit_.*$", "edit_file"));
        assert!(!matches("^edit_.*$", "read_file"));
        assert!(matches("file$", "edit_file"));
        assert!(matches("(read|write)_file", "write_file"));
    }

    #[test]
    fn an_invalid_regex_skips_the_hook_instead_of_panicking() {
        assert!(!matches("edit_file[", "edit_file"));
        assert!(!matches("*(", "anything"));
    }

    // ---- item 73: the in-process `if` gate ----

    #[test]
    fn glob_matching_handles_stars_and_question_marks() {
        assert!(glob_match("cargo *", "cargo build"));
        assert!(glob_match("*.rs", "src/engine/hooks.rs"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("cargo *", "npm install"));
        assert!(glob_match("*build*", "cargo build --release"));
    }

    #[test]
    fn a_precondition_gates_on_tool_then_glob() {
        let bash_args = r#"{"command":"cargo build"}"#;
        assert!(precondition_holds("bash(cargo *)", "bash", bash_args));
        // Right glob, wrong tool: never forks.
        assert!(!precondition_holds("bash(cargo *)", "edit_file", bash_args));
        // Right tool, wrong glob.
        assert!(!precondition_holds(
            "bash(cargo *)",
            "bash",
            r#"{"command":"npm test"}"#
        ));
    }

    #[test]
    fn a_precondition_reaches_nested_string_values() {
        assert!(precondition_holds(
            "edit_file(*.rs)",
            "edit_file",
            r#"{"path":"src/main.rs","old_string":"x"}"#
        ));
        assert!(!precondition_holds(
            "edit_file(*.rs)",
            "edit_file",
            r#"{"path":"README.md"}"#
        ));
    }

    #[test]
    fn a_bare_glob_precondition_skips_the_tool_check() {
        assert!(precondition_holds("cargo *", "bash", r#"{"command":"cargo fmt"}"#));
        assert!(precondition_holds("", "anything", "{}"));
    }

    #[test]
    fn a_precondition_on_unparseable_arguments_matches_the_raw_string() {
        assert!(precondition_holds("*cargo*", "bash", "not json but cargo appears"));
        assert!(!precondition_holds("*cargo*", "bash", "not json at all"));
    }

    // ---- item 72: the config snapshot and kill switch ----

    fn snap(lines: &[(&str, &str)], disabled: bool) -> HookSnapshot {
        HookSnapshot::from_entries(
            lines.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            disabled,
        )
    }

    #[test]
    fn a_snapshot_keeps_runnable_hooks_and_drops_the_rest() {
        let s = snap(
            &[
                ("pre_tool", "name=guard matcher=bash cmd=./guard.sh"),
                ("post_tool", "name=fmt matcher=edit_file cmd='cargo fmt'"),
                ("nonsense", "cmd=x"),
                ("stop", "matcher=* timeout=5"),
            ],
            false,
        );
        assert_eq!(s.hooks.len(), 2);
        assert_eq!(s.select(HookEvent::PreTool, "bash").len(), 1);
        assert!(s.select(HookEvent::PreTool, "read_file").is_empty());
        assert_eq!(s.select(HookEvent::PostTool, "edit_file")[0].name, "fmt");
    }

    #[test]
    fn the_kill_switch_selects_nothing_even_with_hooks_configured() {
        let s = snap(&[("pre_tool", "name=guard cmd=./guard.sh")], true);
        assert_eq!(s.hooks.len(), 1, "still parsed, so `hooks list` can show them");
        assert!(s.select(HookEvent::PreTool, "bash").is_empty());
    }

    #[test]
    fn selection_preserves_config_order() {
        let s = snap(
            &[
                ("pre_tool", "name=first cmd=a"),
                ("pre_tool", "name=second cmd=b"),
            ],
            false,
        );
        let names: Vec<&str> = s
            .select(HookEvent::PreTool, "bash")
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(names, ["first", "second"]);
    }

    // ---- item 71: once / async / asyncRewake ----

    #[test]
    fn parse_reads_the_lifecycle_flags() {
        let h = HookDef::parse("session_start", "cmd=x once=t async=t", 0).unwrap();
        assert!(h.once);
        assert!(h.run_async);
        assert!(!h.async_rewake);

        // asyncRewake implies async: rewaking is what an async exit 2 does.
        let r = HookDef::parse("post_tool", "cmd=x asyncRewake=t", 0).unwrap();
        assert!(r.async_rewake);
        assert!(r.run_async);

        let plain = HookDef::parse("stop", "cmd=x once=nil", 0).unwrap();
        assert!(!plain.once);
        assert!(!plain.run_async);
    }

    #[test]
    fn once_claims_a_single_run_per_name() {
        let mut h = cmd_hook("true");
        h.name = "once-claim-test".into();
        h.once = true;
        assert!(claim_once(&h));
        assert!(!claim_once(&h), "second claim is refused");

        let mut repeatable = h.clone();
        repeatable.name = "repeatable-claim-test".into();
        repeatable.once = false;
        assert!(claim_once(&repeatable));
        assert!(claim_once(&repeatable), "a non-once hook always claims");
    }

    async fn fire_test(s: &HookSnapshot, tool: &str, args: &str) -> HookRun {
        fire_with(s, HookEvent::PreTool, HookPayload::new(HookEvent::PreTool), tool, args, None, None).await
    }

    #[tokio::test]
    async fn a_run_collects_context_and_stops_at_the_first_block() {
        let s = snap(
            &[
                ("pre_tool", "name=a cmd='echo first'"),
                ("pre_tool", "name=b cmd=\"echo 'nope' >&2; exit 2\""),
                ("pre_tool", "name=c cmd='echo never-runs'"),
            ],
            false,
        );
        let run = fire_test(&s, "bash", "{}").await;
        assert!(run.blocked());
        assert_eq!(run.blocked.as_deref(), Some("nope"));
        assert_eq!(run.context, ["first"]);
        assert_eq!(run.context_block().as_deref(), Some("first"));
        assert_eq!(run.outcomes.len(), 2, "the hook after the block never ran");
    }

    #[tokio::test]
    async fn the_kill_switch_fires_nothing() {
        let s = snap(&[("pre_tool", "name=k cmd=\"exit 2\"")], true);
        let run = fire_test(&s, "bash", "{}").await;
        assert!(!run.blocked());
        assert!(run.outcomes.is_empty());
    }

    #[tokio::test]
    async fn a_failed_precondition_skips_the_hook_without_forking() {
        let s = snap(
            &[("pre_tool", "name=fmt if='bash(cargo *)' cmd=\"exit 2\"")],
            false,
        );
        let skipped = fire_test(&s, "bash", r#"{"command":"npm test"}"#).await;
        assert!(!skipped.blocked());
        assert!(skipped.outcomes.is_empty());

        let fired = fire_test(&s, "bash", r#"{"command":"cargo build"}"#).await;
        assert!(fired.blocked());
    }

    #[tokio::test]
    async fn an_async_hook_does_not_block_the_caller() {
        let s = snap(&[("pre_tool", "name=bg async=t cmd=\"sleep 5; exit 2\"")], false);
        let run = tokio::time::timeout(Duration::from_secs(1), fire_test(&s, "bash", "{}"))
            .await
            .expect("fire returns immediately, sleep still running");
        assert!(!run.blocked(), "a backgrounded hook cannot block");
        assert!(run.outcomes.is_empty());
    }

    #[tokio::test]
    async fn async_rewake_pushes_its_reason_onto_the_steer_queue() {
        let s = snap(
            &[(
                "post_tool",
                "name=lint asyncRewake=t cmd=\"echo 'clippy is angry' >&2; exit 2\"",
            )],
            false,
        );
        let steer: crate::engine::SteerQueue = Default::default();
        let run = fire_with(
            &s,
            HookEvent::PostTool,
            HookPayload::new(HookEvent::PostTool),
            "edit_file",
            "{}",
            Some(&steer),
            None,
        )
        .await;
        assert!(!run.blocked(), "rewake steers the smith, it does not block");

        for _ in 0..100 {
            if !steer.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let queued = steer.lock().unwrap().clone();
        assert_eq!(queued.len(), 1, "expected one steer message, got {queued:?}");
        assert!(queued[0].contains("clippy is angry"), "{}", queued[0]);
        assert!(queued[0].contains("lint"), "{}", queued[0]);
    }

    #[tokio::test]
    async fn a_once_hook_fires_a_single_time_across_events() {
        let s = snap(
            &[("pre_tool", "name=once-fire-test once=t cmd='echo hello'")],
            false,
        );
        let first = fire_test(&s, "bash", "{}").await;
        assert_eq!(first.context, ["hello"]);
        let second = fire_test(&s, "bash", "{}").await;
        assert!(second.context.is_empty(), "spent hooks drop out of the session");
    }

    #[test]
    fn an_empty_run_contributes_no_context() {
        assert!(HookRun::default().context_block().is_none());
        assert!(!HookRun::default().blocked());
        assert!(HookRun::default().rewritten_arguments().is_none());
    }

    // ---- item 75: structured PreToolUse stdout ----

    #[test]
    fn structured_stdout_splits_into_rewrite_and_injected_context() {
        let o = classify(
            "guard",
            0,
            r#"{"updated_input":{"command":"ls -la"},"additional_context":"rewrote a dangerous call"}"#,
            "",
            4,
        );
        assert_eq!(o.updated_input.as_ref().unwrap()["command"], "ls -la");
        assert_eq!(o.additional_context.as_deref(), Some("rewrote a dangerous call"));
        assert!(o.context.is_none(), "structured output is not also raw context");
    }

    #[test]
    fn either_structured_key_works_alone_in_both_spellings() {
        let ctx_only = classify("lint", 0, r#"{"additional_context":"3 warnings"}"#, "", 1);
        assert_eq!(ctx_only.additional_context.as_deref(), Some("3 warnings"));
        assert!(ctx_only.updated_input.is_none());

        let camel = classify("guard", 0, r#"{"updatedInput":{"path":"safe.txt"}}"#, "", 1);
        assert_eq!(camel.updated_input.as_ref().unwrap()["path"], "safe.txt");
        assert!(camel.additional_context.is_none());
    }

    #[test]
    fn plain_and_unrelated_json_stay_raw_context() {
        let plain = classify("lint", 0, "just some text", "", 1);
        assert_eq!(plain.context.as_deref(), Some("just some text"));
        assert!(plain.updated_input.is_none());

        // Valid JSON with neither key is output, not a directive.
        let other = classify("lint", 0, r#"{"count":3}"#, "", 1);
        assert_eq!(other.context.as_deref(), Some(r#"{"count":3}"#));
        assert!(other.additional_context.is_none());
    }

    #[tokio::test]
    async fn a_pre_tool_hook_rewrites_arguments_and_injects_context() {
        let s = snap(
            &[(
                "pre_tool",
                r#"name=guard cmd='echo {\"updated_input\":{\"command\":\"ls\"},\"additional_context\":\"rm was rewritten\"}'"#,
            )],
            false,
        );
        let run = fire_test(&s, "bash", r#"{"command":"rm -rf /"}"#).await;
        assert!(!run.blocked());
        assert_eq!(run.rewritten_arguments().as_deref(), Some(r#"{"command":"ls"}"#));
        assert_eq!(run.context, ["rm was rewritten"]);
    }

    #[tokio::test]
    async fn the_last_rewrite_wins_and_a_non_object_is_refused() {
        let s = snap(
            &[
                ("pre_tool", r#"name=a cmd='echo {\"updated_input\":{\"n\":1}}'"#),
                ("pre_tool", r#"name=b cmd='echo {\"updated_input\":{\"n\":2}}'"#),
            ],
            false,
        );
        let run = fire_test(&s, "bash", "{}").await;
        assert_eq!(run.rewritten_arguments().as_deref(), Some(r#"{"n":2}"#));

        let bogus = HookRun {
            updated_input: Some(serde_json::json!("not an object")),
            ..Default::default()
        };
        assert!(bogus.rewritten_arguments().is_none());
    }

    // ---- items 74 and 76: prompt, agent, and HTTP kinds ----

    /// Scripted provider for the prompt-hook path. judge.rs has its own
    /// mock, private to its tests module, so this one is local.
    struct MockGate(std::sync::Mutex<std::collections::VecDeque<String>>);

    impl MockGate {
        fn new(replies: &[&str]) -> Self {
            Self(std::sync::Mutex::new(
                replies.iter().map(|s| s.to_string()).collect(),
            ))
        }
    }

    impl crate::engine::Provider for MockGate {
        fn chat(
            &self,
            _req: crate::engine::ChatRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<crate::engine::NormalizedResponse, crate::error::SlagError>,
                    > + Send
                    + '_,
            >,
        > {
            let content = self.0.lock().unwrap().pop_front().unwrap_or_default();
            Box::pin(async move {
                Ok(crate::engine::NormalizedResponse {
                    model: None,
                    content,
                    tool_calls: vec![],
                    finish_reason: crate::engine::FinishReason::Stop,
                    reasoning: None,
                    reasoning_details: None,
                    usage: Default::default(),
                })
            })
        }
    }

    fn kind_hook(kind: HookKind) -> HookDef {
        HookDef {
            kind,
            ..cmd_hook("unused")
        }
    }

    fn gate_payload() -> HookPayload {
        HookPayload {
            event: "pre_tool".into(),
            tool_name: Some("bash".into()),
            ..Default::default()
        }
    }

    #[test]
    fn kind_parses_all_four_spellings() {
        let c = HookDef::parse("pre_tool", "cmd=./guard.sh", 0).expect("cmd");
        assert_eq!(c.kind, HookKind::Command("./guard.sh".into()));

        let p = HookDef::parse("pre_tool", "prompt='refuse writes' model=x/y", 0).expect("prompt");
        assert_eq!(
            p.kind,
            HookKind::Prompt {
                prompt: "refuse writes".into(),
                model: Some("x/y".into()),
            }
        );

        let a = HookDef::parse("post_tool", "agent='verify the diff'", 0).expect("agent");
        assert_eq!(
            a.kind,
            HookKind::Agent {
                prompt: "verify the diff".into(),
                model: None,
            }
        );

        let h = HookDef::parse(
            "stop",
            "url=https://hooks.example/slag header='authorization: Bearer $TOK' allowedEnvVars=TOK",
            0,
        )
        .expect("url");
        assert_eq!(
            h.kind,
            HookKind::Http {
                url: "https://hooks.example/slag".into(),
                headers: vec![("authorization".into(), "Bearer $TOK".into())],
                allowed_env: vec!["TOK".into()],
            }
        );
    }

    #[test]
    fn ambiguous_kind_is_refused() {
        // Two kinds on one line: guessing which the operator meant is
        // worse than dropping the hook.
        assert!(HookDef::parse("pre_tool", "cmd=./x.sh prompt='refuse'", 0).is_none());
        assert!(HookDef::parse("stop", "agent='check' url=https://e.example", 0).is_none());
    }

    #[tokio::test]
    async fn prompt_hook_blocks_with_the_models_reason() {
        let gate = MockGate::new(&[r#"{"decision":"block","reason":"touches .env"}"#]);
        let def = kind_hook(HookKind::Prompt {
            prompt: "refuse secret reads".into(),
            model: None,
        });
        let out = run_prompt(&def, &gate_payload(), &gate, "x/y", "refuse secret reads").await;
        assert_eq!(out.code, 2);
        assert_eq!(out.block.as_deref(), Some("touches .env"));
    }

    #[tokio::test]
    async fn prompt_hook_allows_and_turns_the_reason_into_context() {
        let gate = MockGate::new(&[r#"{"decision":"allow","reason":"reads a test fixture"}"#]);
        let def = kind_hook(HookKind::Prompt {
            prompt: "gate".into(),
            model: None,
        });
        let out = run_prompt(&def, &gate_payload(), &gate, "x/y", "gate").await;
        assert_eq!(out.code, 0);
        assert!(out.block.is_none());
        assert_eq!(out.context.as_deref(), Some("reads a test fixture"));
    }

    #[tokio::test]
    async fn prompt_hook_failure_never_blocks() {
        // Two malformed replies: judge::rule errors, and a gate that
        // cannot rule must not wedge the tool call.
        let gate = MockGate::new(&["nope", "still nope"]);
        let def = kind_hook(HookKind::Prompt {
            prompt: "gate".into(),
            model: None,
        });
        let out = run_prompt(&def, &gate_payload(), &gate, "x/y", "gate").await;
        assert_eq!(out.code, CODE_FAILED);
        assert!(out.block.is_none());
    }

    #[tokio::test]
    async fn agent_hook_refuses_on_a_block_line() {
        let smith = crate::smith::mock::MockSmith::fixed("looked at it\nBLOCK: the proof is fake");
        let def = kind_hook(HookKind::Agent {
            prompt: "verify".into(),
            model: None,
        });
        let out = run_agent(&def, &gate_payload(), &smith, "verify").await;
        assert_eq!(out.code, 2);
        assert_eq!(out.block.as_deref(), Some("the proof is fake"));
    }

    #[tokio::test]
    async fn agent_hook_treats_plain_output_as_context() {
        let smith = crate::smith::mock::MockSmith::fixed("the diff looks fine");
        let def = kind_hook(HookKind::Agent {
            prompt: "verify".into(),
            model: None,
        });
        let out = run_agent(&def, &gate_payload(), &smith, "verify").await;
        assert_eq!(out.code, 0);
        assert!(out.block.is_none());
        assert_eq!(out.context.as_deref(), Some("the diff looks fine"));
    }

    #[tokio::test]
    async fn agent_hook_failure_never_blocks() {
        let smith = crate::smith::mock::MockSmith::failing();
        let def = kind_hook(HookKind::Agent {
            prompt: "verify".into(),
            model: None,
        });
        let out = run_agent(&def, &gate_payload(), &smith, "verify").await;
        assert_eq!(out.code, CODE_FAILED);
        assert!(out.block.is_none());
    }

    #[test]
    fn http_outcome_maps_status_onto_the_exit_protocol() {
        // 2xx: the body is context, exactly like an exit-0 command.
        let ok = http_outcome("n", 200, "deploy queued", 1);
        assert_eq!(ok.code, 0);
        assert_eq!(ok.context.as_deref(), Some("deploy queued"));

        assert_eq!(http_outcome("n", 204, "", 1).code, 0);

        // 403 is the one refusal status; the body is the reason.
        let no = http_outcome("n", 403, "policy: no prod writes", 1);
        assert_eq!(no.code, 2);
        assert_eq!(no.block.as_deref(), Some("policy: no prod writes"));

        // Everything else is the webhook's problem, not the forge's.
        for status in [401, 404, 500, 503] {
            let bad = http_outcome("n", status, "", 1);
            assert_eq!(bad.code, CODE_FAILED, "status {status} must not block");
            assert!(bad.block.is_none());
            assert!(bad.context.unwrap().contains(&status.to_string()));
        }
    }

    #[test]
    fn http_403_without_a_body_still_names_a_reason() {
        // The smith reads `block`; an empty one would refuse silently.
        let out = http_outcome("n", 403, "   ", 1);
        assert_eq!(out.code, 2);
        assert_eq!(out.block.as_deref(), Some("webhook refused the action"));
    }

    #[test]
    fn interpolate_expands_only_allowed_names() {
        // Unique names: env is process-wide and cargo test is parallel.
        std::env::set_var("SLAG_TEST_HOOK_TOKEN_A", "sekret");
        std::env::set_var("SLAG_TEST_HOOK_TOKEN_B", "unlisted");
        let allowed = vec!["SLAG_TEST_HOOK_TOKEN_A".to_string()];

        assert_eq!(
            interpolate("Bearer $SLAG_TEST_HOOK_TOKEN_A", &allowed),
            "Bearer sekret"
        );
        // Named but not listed: empty, not the value, and not the literal.
        assert_eq!(
            interpolate("Bearer $SLAG_TEST_HOOK_TOKEN_B", &allowed),
            "Bearer "
        );
        // Listed but unset resolves empty rather than erroring.
        assert_eq!(
            interpolate("$NOPE", &vec!["NOPE".to_string()]),
            ""
        );
        assert_eq!(interpolate("no dollars here", &allowed), "no dollars here");
    }

    #[test]
    fn interpolate_handles_braces_dollars_and_tails() {
        std::env::set_var("SLAG_TEST_HOOK_TOKEN_C", "v");
        let allowed = vec!["SLAG_TEST_HOOK_TOKEN_C".to_string()];

        assert_eq!(
            interpolate("[${SLAG_TEST_HOOK_TOKEN_C}]", &allowed),
            "[v]"
        );
        assert_eq!(interpolate("$$", &allowed), "$");
        assert_eq!(interpolate("cost: 5$", &allowed), "cost: 5$");
        // Unclosed brace and a bare `$` stay literal instead of eating
        // the rest of the value.
        assert_eq!(interpolate("${UNCLOSED", &allowed), "${UNCLOSED");
        assert_eq!(interpolate("a $ b", &allowed), "a $ b");
        // Expansion does not re-expand: a value containing `$X` is text.
        std::env::set_var("SLAG_TEST_HOOK_TOKEN_D", "$SLAG_TEST_HOOK_TOKEN_C");
        assert_eq!(
            interpolate("$SLAG_TEST_HOOK_TOKEN_D", &vec![
                "SLAG_TEST_HOOK_TOKEN_D".to_string(),
                "SLAG_TEST_HOOK_TOKEN_C".to_string(),
            ]),
            "$SLAG_TEST_HOOK_TOKEN_C"
        );
    }

    #[test]
    fn describe_names_every_kind() {
        let s = snap(
            &[
                ("pre_tool", "name=c cmd=./guard.sh"),
                ("pre_tool", "name=p prompt='refuse writes' model=x/y"),
                ("post_tool", "name=a agent='verify the diff'"),
                ("stop", "name=h url=https://e.example allowedEnvVars=TOK"),
            ],
            false,
        );
        let out = describe(&s);
        assert!(out.contains("cmd: ./guard.sh"));
        assert!(out.contains("prompt: refuse writes"));
        assert!(out.contains("x/y"), "a model override is shown");
        assert!(out.contains("agent: verify the diff"));
        assert!(out.contains("url: https://e.example"));
        assert!(out.contains("env: TOK"), "the allowlist is visible");
    }
}
