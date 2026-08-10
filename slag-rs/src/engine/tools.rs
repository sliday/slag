//! tools — the seven native tools the smith works with.
//!
//! Sandbox: every path resolves inside the anvil root. Edit uses the
//! hermes fuzzy ladder (exact → line-trimmed → whitespace-normalized →
//! indentation-flexible) with escape-drift and replace_all guards.
//! recipe_view is the one sandbox exception: config-dir recipes are
//! trusted local config and read directly.

// Declared here (not in the frozen engine/mod.rs). Integrator: move this
// to `pub mod recipes;` in engine/mod.rs when the freeze lifts and drop
// the #[path] shim.
#[path = "recipes.rs"]
pub mod recipes;

// Same freeze workaround: judge belongs in engine/mod.rs as `pub mod judge;`.
// Move it there and drop this shim when the freeze lifts.
#[path = "judge.rs"]
pub mod judge;

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;

use super::{ToolCall, ToolOutcome, ToolSpec};
use crate::error::SlagError;

const READ_LIMIT_DEFAULT: usize = 2000;
const BASH_TIMEOUT_DEFAULT: u64 = 120;
const BASH_OUTPUT_CAP: usize = 30_000;
const GREP_LINE_CAP: usize = 100;
const DIFF_LINE_CAP: usize = 12;

/// Native toolbox rooted at an anvil worktree.
#[derive(Clone)]
pub struct ToolBox {
    root: PathBuf,
}

impl ToolBox {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = root.into();
        let root = root.canonicalize().unwrap_or(root);
        Self { root }
    }

    /// Tool schemas advertised to the model.
    pub fn specs() -> Vec<ToolSpec> {
        vec![
            spec(
                "read_file",
                "Read a file from the workspace. Output is LINENUM|CONTENT per line.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path relative to workspace root"},
                        "offset": {"type": "integer", "description": "1-based line to start from (default 1)"},
                        "limit": {"type": "integer", "description": "Max lines to return (default 2000)"}
                    },
                    "required": ["path"]
                }),
            ),
            spec(
                "write_file",
                "Create or overwrite a file. The write is verified by re-reading the file.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path relative to workspace root"},
                        "content": {"type": "string", "description": "Full file content"}
                    },
                    "required": ["path", "content"]
                }),
            ),
            spec(
                "edit_file",
                "Replace old_string with new_string in a file. old_string must match exactly once; \
                 fuzzy whitespace/indentation fallbacks apply when exact match fails. \
                 Set replace_all to replace every exact occurrence.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path relative to workspace root"},
                        "old_string": {"type": "string", "description": "Text to replace"},
                        "new_string": {"type": "string", "description": "Replacement text"},
                        "replace_all": {"type": "boolean", "description": "Replace all exact occurrences (default false)"}
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            ),
            spec(
                "bash",
                "Run a shell command in the workspace root. stdout and stderr are merged.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to run"},
                        "timeout": {"type": "integer", "description": "Timeout in seconds (default 120)"}
                    },
                    "required": ["command"]
                }),
            ),
            spec(
                "grep",
                "Search file contents for a regex pattern. Returns file:line:content matches.",
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Regex pattern"},
                        "path": {"type": "string", "description": "Directory or file to search (default workspace root)"}
                    },
                    "required": ["pattern"]
                }),
            ),
            spec(
                "recipe_view",
                "Load a recipe's full instructions by name. Recipe names come from the \
                 Recipes index in the system prompt.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Recipe name from the Recipes index"}
                    },
                    "required": ["name"]
                }),
            ),
            spec(
                "finish",
                "Finish the task and report what was done. Call this exactly once, when the work is complete.",
                json!({
                    "type": "object",
                    "properties": {
                        "summary": {"type": "string", "description": "Summary of the completed work"}
                    },
                    "required": ["summary"]
                }),
            ),
        ]
    }

    /// (path, is_writer) for path-touching tools; None for
    /// bash/grep/recipe_view/finish (no scheduling constraint).
    /// The agent dispatcher uses this for reader-writer scheduling.
    pub fn path_access(call: &ToolCall) -> Option<(String, bool)> {
        let is_writer = match call.name.as_str() {
            "read_file" => false,
            "write_file" | "edit_file" => true,
            _ => return None,
        };
        let args: Value = serde_json::from_str(&call.arguments).ok()?;
        let path = args.get("path")?.as_str()?.to_string();
        Some((path, is_writer))
    }

    pub async fn dispatch(&self, call: &ToolCall) -> ToolOutcome {
        match self.run(call).await {
            Ok(output) => ToolOutcome { output, is_error: false },
            Err(e) => ToolOutcome { output: e.to_string(), is_error: true },
        }
    }

    async fn run(&self, call: &ToolCall) -> Result<String, SlagError> {
        let args: Value = serde_json::from_str(&call.arguments)
            .map_err(|e| SlagError::Tool(format!("bad JSON arguments for {}: {e}", call.name)))?;
        match call.name.as_str() {
            "read_file" => self.read_file(&args).await,
            "write_file" => self.write_file(&args).await,
            "edit_file" => self.edit_file(&args).await,
            "bash" => self.bash(&args).await,
            "grep" => self.grep(&args).await,
            "recipe_view" => self.recipe_view(&args).await,
            "finish" => Ok(req_str(&args, "finish", "summary")?.to_string()),
            other => Err(SlagError::Tool(format!("unknown tool: {other}"))),
        }
    }

    /// Resolve a path argument inside the sandbox root.
    /// Lexical normalization first, then symlink resolution: canonicalize
    /// the deepest existing ancestor and re-check containment, so a symlink
    /// inside the root cannot smuggle reads/writes outside it.
    fn resolve(&self, raw: &str) -> Result<PathBuf, SlagError> {
        let p = Path::new(raw);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };
        let mut normal = PathBuf::new();
        for comp in joined.components() {
            match comp {
                Component::ParentDir => {
                    if !normal.pop() {
                        return Err(SlagError::Tool(format!("path escapes workspace: {raw}")));
                    }
                }
                Component::CurDir => {}
                c => normal.push(c.as_os_str()),
            }
        }
        if !normal.starts_with(&self.root) {
            return Err(SlagError::Tool(format!("path escapes workspace: {raw}")));
        }

        // Symlink check: split off not-yet-existing tail components
        // (dangling symlinks count as existing so they get resolved and
        // rejected rather than silently followed on write).
        let mut existing = normal.clone();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        while std::fs::symlink_metadata(&existing).is_err() {
            match existing.file_name() {
                Some(name) => {
                    tail.push(name.to_os_string());
                    existing.pop();
                }
                None => break,
            }
        }
        let canon = existing
            .canonicalize()
            .map_err(|e| SlagError::Tool(format!("cannot resolve path {raw}: {e}")))?;
        let mut real = canon;
        for name in tail.iter().rev() {
            real.push(name);
        }
        if !real.starts_with(&self.root) {
            return Err(SlagError::Tool(format!("path escapes workspace: {raw}")));
        }
        Ok(real)
    }

    async fn read_file(&self, args: &Value) -> Result<String, SlagError> {
        let raw = req_str(args, "read_file", "path")?;
        let path = self.resolve(raw)?;
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .map(|v| v.max(1) as usize)
            .unwrap_or(1);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(READ_LIMIT_DEFAULT)
            .max(1);

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| SlagError::Tool(format!("cannot read {raw}: {e}")))?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return Ok("(empty file)".into());
        }
        if offset > lines.len() {
            return Ok(format!(
                "(file has {} lines; offset {} is past the end)",
                lines.len(),
                offset
            ));
        }
        let end = (offset - 1).saturating_add(limit).min(lines.len());
        let mut out = String::new();
        for (i, line) in lines[offset - 1..end].iter().enumerate() {
            out.push_str(&format!("{}|{}\n", offset + i, line));
        }
        if end < lines.len() {
            out.push_str(&format!(
                "(truncated: {} more lines, next offset: {})",
                lines.len() - end,
                end + 1
            ));
        } else {
            out.pop();
        }
        Ok(out)
    }

    async fn write_file(&self, args: &Value) -> Result<String, SlagError> {
        let raw = req_str(args, "write_file", "path")?;
        let content = req_str(args, "write_file", "content")?;
        let path = self.resolve(raw)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| SlagError::Tool(format!("cannot create parent dirs for {raw}: {e}")))?;
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|e| SlagError::Tool(format!("cannot write {raw}: {e}")))?;
        // Verified-write (hermes pattern): re-read from disk and hash.
        let back = tokio::fs::read(&path)
            .await
            .map_err(|e| SlagError::Tool(format!("write verification re-read failed for {raw}: {e}")))?;
        if back != content.as_bytes() {
            return Err(SlagError::Tool(format!(
                "write verification failed for {raw}: on-disk bytes differ"
            )));
        }
        Ok(format!("verified: true (checksum {})", &checksum_hex(&back)[..12]))
    }

    async fn edit_file(&self, args: &Value) -> Result<String, SlagError> {
        let raw = req_str(args, "edit_file", "path")?;
        let old = req_str(args, "edit_file", "old_string")?;
        let new = req_str(args, "edit_file", "new_string")?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if old.is_empty() {
            return Err(SlagError::Tool("old_string must not be empty".into()));
        }
        let path = self.resolve(raw)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| SlagError::Tool(format!("cannot read {raw}: {e}")))?;

        if old == new {
            return Ok("already applied (old_string equals new_string)".into());
        }

        let matches = find_all(&content, old);

        if replace_all {
            if matches.is_empty() {
                if content.contains(new) {
                    return Ok("already applied (new_string present, old_string absent)".into());
                }
                return Err(SlagError::Tool(format!(
                    "no exact match for old_string in {raw}; fuzzy matching is refused with replace_all=true.\n{}",
                    near_miss_hint(&content, old)
                )));
            }
            let new_content = content.replace(old, new);
            tokio::fs::write(&path, &new_content)
                .await
                .map_err(|e| SlagError::Tool(format!("cannot write {raw}: {e}")))?;
            return Ok(format!(
                "replaced {} occurrence(s) in {raw} (exact)",
                matches.len()
            ));
        }

        if matches.len() > 1 {
            let lines: Vec<String> = matches
                .iter()
                .map(|off| line_of(&content, *off).to_string())
                .collect();
            return Err(SlagError::Tool(format!(
                "old_string matches {} times in {raw} (lines {}); provide more context or set replace_all",
                matches.len(),
                lines.join(", ")
            )));
        }

        let (new_content, strategy, at_line, removed, added) = if let Some(&off) = matches.first() {
            let mut nc = String::with_capacity(content.len() - old.len() + new.len());
            nc.push_str(&content[..off]);
            nc.push_str(new);
            nc.push_str(&content[off + old.len()..]);
            (
                nc,
                "exact",
                line_of(&content, off),
                old.lines().map(String::from).collect::<Vec<_>>(),
                new.lines().map(String::from).collect::<Vec<_>>(),
            )
        } else {
            if content.contains(new) {
                return Ok("already applied (new_string present, old_string absent)".into());
            }
            let file_lines: Vec<&str> = content.lines().collect();
            let needle_lines: Vec<&str> = old.lines().collect();
            let m = fuzzy_find(&file_lines, &needle_lines).ok_or_else(|| {
                SlagError::Tool(format!(
                    "no match for old_string in {raw}.\n{}",
                    near_miss_hint(&content, old)
                ))
            })?;
            // Escape-drift guard: a fuzzy match plus stray escapes from JSON
            // serialization would corrupt the write.
            if escape_drift(old, new) {
                return Err(SlagError::Tool(format!(
                    "refused: matched via {} strategy but new_string contains escaped quote \
                     sequences (\\' or \\\") absent from old_string — likely JSON escaping drift. \
                     Re-send the edit with exact literal text.",
                    m.strategy
                )));
            }
            let removed: Vec<String> = file_lines[m.start..m.start + m.len]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let added: Vec<String> = match &m.indent {
                Some(indent) => reindent(new, indent),
                None => new.lines().map(String::from).collect(),
            };
            let mut out_lines: Vec<String> = Vec::with_capacity(file_lines.len());
            out_lines.extend(file_lines[..m.start].iter().map(|s| s.to_string()));
            out_lines.extend(added.iter().cloned());
            out_lines.extend(file_lines[m.start + m.len..].iter().map(|s| s.to_string()));
            // lines() strips \r; rejoin with the file's own line ending so a
            // fuzzy edit never rewrites every EOL in a CRLF file.
            let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
            let mut nc = out_lines.join(eol);
            if content.ends_with('\n') {
                nc.push_str(eol);
            }
            (nc, m.strategy, m.start + 1, removed, added)
        };

        if new_content == content {
            return Ok("already applied (edit produces no change)".into());
        }

        tokio::fs::write(&path, &new_content)
            .await
            .map_err(|e| SlagError::Tool(format!("cannot write {raw}: {e}")))?;

        Ok(diff_summary(raw, strategy, at_line, &removed, &added))
    }

    async fn bash(&self, args: &Value) -> Result<String, SlagError> {
        let command = req_str(args, "bash", "command")?;
        let timeout = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(BASH_TIMEOUT_DEFAULT)
            .max(1);
        self.run_shell(command, timeout).await
    }

    async fn run_shell(&self, command: &str, timeout_secs: u64) -> Result<String, SlagError> {
        let child = Command::new("sh")
            .arg("-lc")
            .arg(command)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SlagError::Tool(format!("failed to spawn shell: {e}")))?;

        let waited =
            tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await;
        match waited {
            Ok(Ok(output)) => {
                let mut merged = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    if !merged.is_empty() && !merged.ends_with('\n') {
                        merged.push('\n');
                    }
                    merged.push_str(&stderr);
                }
                let mut out = truncate_tail(&merged, BASH_OUTPUT_CAP);
                if !output.status.success() {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&format!("(exit {})", output.status.code().unwrap_or(-1)));
                }
                Ok(out)
            }
            Ok(Err(e)) => Err(SlagError::Tool(format!("shell wait failed: {e}"))),
            // kill_on_drop reaps the child when the timed-out future is dropped.
            Err(_) => Err(SlagError::Tool(format!(
                "command timed out after {timeout_secs}s and was killed"
            ))),
        }
    }

    async fn grep(&self, args: &Value) -> Result<String, SlagError> {
        let pattern = req_str(args, "grep", "pattern")?;
        let raw_path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let path = self.resolve(raw_path)?;
        let cmd = format!(
            "if command -v rg >/dev/null 2>&1; then rg -n --no-heading -e {p} {d}; else grep -rn -e {p} {d}; fi",
            p = sh_quote(pattern),
            d = sh_quote(&path.display().to_string()),
        );
        let out = self.run_shell(&cmd, BASH_TIMEOUT_DEFAULT).await?;
        let lines: Vec<&str> = out.lines().collect();
        if lines.is_empty() || (lines.len() == 1 && lines[0].starts_with("(exit ")) {
            return Ok("no matches found".into());
        }
        let mut result = lines
            .iter()
            .take(GREP_LINE_CAP)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        if lines.len() > GREP_LINE_CAP {
            result.push_str(&format!(
                "\n(truncated: {} more lines)",
                lines.len() - GREP_LINE_CAP
            ));
        }
        Ok(result)
    }

    /// Load a recipe's full RECIPE.md by name. Workspace recipes resolve
    /// through the sandbox as usual; config-dir recipes are trusted local
    /// config and bypass it. A name collision refuses the bare name
    /// (loud-collision pattern) — rename one of the recipes to proceed.
    async fn recipe_view(&self, args: &Value) -> Result<String, SlagError> {
        let name = req_str(args, "recipe_view", "name")?;
        let found = recipes::discover(&self.root);
        let hits: Vec<&recipes::Recipe> = found.iter().filter(|r| r.name == name).collect();
        match hits.len() {
            0 => {
                let known: Vec<&str> = found.iter().map(|r| r.name.as_str()).collect();
                Err(SlagError::Tool(format!(
                    "unknown recipe '{name}'; known recipes: {}",
                    if known.is_empty() {
                        "(none)".to_string()
                    } else {
                        known.join(", ")
                    }
                )))
            }
            1 => {
                let recipe = hits[0];
                let path = if recipe.path.starts_with(&self.root) {
                    self.resolve(&recipe.path.display().to_string())?
                } else {
                    recipe.path.clone()
                };
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|e| SlagError::Tool(format!("cannot read recipe '{name}': {e}")))
            }
            n => {
                let paths: Vec<String> =
                    hits.iter().map(|r| r.path.display().to_string()).collect();
                Err(SlagError::Tool(format!(
                    "name collision: {n} recipes named '{name}' ({}); rename one and retry",
                    paths.join(", ")
                )))
            }
        }
    }
}

fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    }
}

fn req_str<'a>(args: &'a Value, tool: &str, key: &str) -> Result<&'a str, SlagError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| SlagError::Tool(format!("{tool}: missing required string argument '{key}'")))
}

/// FNV-1a 64-bit, hex. Labeled "checksum" — integrity marker, not crypto.
fn checksum_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn find_all(haystack: &str, needle: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        offsets.push(start + pos);
        start += pos + needle.len();
    }
    offsets
}

fn line_of(content: &str, offset: usize) -> usize {
    content[..offset].matches('\n').count() + 1
}

fn escape_drift(old: &str, new: &str) -> bool {
    [r"\'", "\\\""]
        .iter()
        .any(|seq| new.contains(seq) && !old.contains(seq))
}

struct FuzzyMatch {
    start: usize,
    len: usize,
    strategy: &'static str,
    /// Set by the indentation-flexible strategy: re-indent new_string to this.
    indent: Option<String>,
}

/// The ladder: line-trimmed → whitespace-normalized → indentation-flexible.
/// Each strategy applies only on a unique match; ambiguity falls through.
fn fuzzy_find(file_lines: &[&str], needle_lines: &[&str]) -> Option<FuzzyMatch> {
    let n = needle_lines.len();
    if n == 0 || n > file_lines.len() {
        return None;
    }

    // (b) line-trimmed
    let starts = scan(file_lines, needle_lines, |a, b| a.trim() == b.trim());
    if starts.len() == 1 {
        return Some(FuzzyMatch {
            start: starts[0],
            len: n,
            strategy: "line-trimmed",
            indent: None,
        });
    }

    // (c) whitespace-normalized
    let starts = scan(file_lines, needle_lines, |a, b| {
        normalize_ws(a) == normalize_ws(b)
    });
    if starts.len() == 1 {
        return Some(FuzzyMatch {
            start: starts[0],
            len: n,
            strategy: "whitespace-normalized",
            indent: None,
        });
    }

    // (d) indentation-flexible: dedent both sides, compare exactly.
    let needle_indent = common_indent(needle_lines);
    let dedented_needle: Vec<&str> = needle_lines
        .iter()
        .map(|l| dedent(l, &needle_indent))
        .collect();
    let mut hits: Vec<(usize, String)> = Vec::new();
    for start in 0..=file_lines.len() - n {
        let window = &file_lines[start..start + n];
        let indent = common_indent(window);
        let ok = window
            .iter()
            .zip(dedented_needle.iter())
            .all(|(w, d)| dedent(w, &indent) == *d);
        if ok {
            hits.push((start, indent));
        }
    }
    if hits.len() == 1 {
        let (start, indent) = hits.remove(0);
        return Some(FuzzyMatch {
            start,
            len: n,
            strategy: "indentation-flexible",
            indent: Some(indent),
        });
    }

    None
}

fn scan(file_lines: &[&str], needle_lines: &[&str], eq: impl Fn(&str, &str) -> bool) -> Vec<usize> {
    let n = needle_lines.len();
    (0..=file_lines.len() - n)
        .filter(|&start| {
            file_lines[start..start + n]
                .iter()
                .zip(needle_lines.iter())
                .all(|(a, b)| eq(a, b))
        })
        .collect()
}

/// Collapse runs of spaces/tabs to a single space; trim ends.
fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.trim().chars() {
        if c == ' ' || c == '\t' {
            if !in_ws {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// Longest common leading-whitespace prefix across non-blank lines.
fn common_indent(lines: &[&str]) -> String {
    let mut common: Option<String> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let ws: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        common = Some(match common {
            None => ws,
            Some(prev) => prev
                .chars()
                .zip(ws.chars())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    common.unwrap_or_default()
}

fn dedent<'a>(line: &'a str, indent: &str) -> &'a str {
    line.strip_prefix(indent).unwrap_or(line)
}

/// Dedent new_string by its own common indent, then apply the target indent.
fn reindent(new: &str, indent: &str) -> Vec<String> {
    let lines: Vec<&str> = new.lines().collect();
    let own = common_indent(&lines);
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                format!("{indent}{}", dedent(l, &own))
            }
        })
        .collect()
}

/// Closest 3 lines by similarity, whitespace visualized (· space, → tab).
fn near_miss_hint(content: &str, old: &str) -> String {
    let probe = old
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(old)
        .trim();
    let mut scored: Vec<(f64, usize, &str)> = content
        .lines()
        .enumerate()
        .map(|(i, line)| (similarity(line.trim(), probe), i + 1, line))
        .filter(|(score, _, _)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    if scored.is_empty() {
        return "no similar lines found".into();
    }
    let mut out = String::from("closest lines (whitespace shown as · and →):");
    for (_, num, line) in scored.iter().take(3) {
        out.push_str(&format!(
            "\n  {num}: {}",
            line.replace(' ', "·").replace('\t', "→")
        ));
    }
    out
}

/// LCS-ratio similarity in [0, 1], capped input length for O(n*m) safety.
fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().take(200).collect();
    let b: Vec<char> = b.chars().take(200).collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    for ca in &a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = if ca == cb {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        prev = cur;
    }
    2.0 * prev[b.len()] as f64 / (a.len() + b.len()) as f64
}

fn diff_summary(
    path: &str,
    strategy: &str,
    line: usize,
    removed: &[String],
    added: &[String],
) -> String {
    let mut out = format!(
        "edited {path} ({strategy})\n@@ -{line},{} +{line},{} @@",
        removed.len(),
        added.len()
    );
    for l in removed.iter().take(DIFF_LINE_CAP) {
        out.push_str(&format!("\n-{l}"));
    }
    if removed.len() > DIFF_LINE_CAP {
        out.push_str(&format!("\n… {} more removed", removed.len() - DIFF_LINE_CAP));
    }
    for l in added.iter().take(DIFF_LINE_CAP) {
        out.push_str(&format!("\n+{l}"));
    }
    if added.len() > DIFF_LINE_CAP {
        out.push_str(&format!("\n… {} more added", added.len() - DIFF_LINE_CAP));
    }
    out
}

fn truncate_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = s.len() - max;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    format!("[truncated]\n{}", &s[cut..])
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    fn setup() -> (tempfile::TempDir, ToolBox) {
        let dir = tempfile::tempdir().expect("tempdir");
        let toolbox = ToolBox::new(dir.path());
        (dir, toolbox)
    }

    fn write(dir: &tempfile::TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).expect("write fixture");
        path
    }

    #[tokio::test]
    async fn read_file_happy() {
        let (dir, tb) = setup();
        write(&dir, "a.txt", "alpha\nbeta\ngamma\n");
        let out = tb.dispatch(&call("read_file", json!({"path": "a.txt"}))).await;
        assert!(!out.is_error);
        assert_eq!(out.output, "1|alpha\n2|beta\n3|gamma");
    }

    #[tokio::test]
    async fn read_file_truncation_notes_next_offset() {
        let (dir, tb) = setup();
        write(&dir, "a.txt", "l1\nl2\nl3\nl4\nl5\n");
        let out = tb
            .dispatch(&call(
                "read_file",
                json!({"path": "a.txt", "offset": 2, "limit": 2}),
            ))
            .await;
        assert!(!out.is_error);
        assert!(out.output.contains("2|l2"));
        assert!(out.output.contains("3|l3"));
        assert!(out.output.contains("next offset: 4"));
    }

    #[tokio::test]
    async fn write_file_happy_verified() {
        let (dir, tb) = setup();
        let out = tb
            .dispatch(&call(
                "write_file",
                json!({"path": "sub/dir/new.txt", "content": "forged\n"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.starts_with("verified: true (checksum "));
        let disk = std::fs::read_to_string(dir.path().join("sub/dir/new.txt")).unwrap();
        assert_eq!(disk, "forged\n");
    }

    #[tokio::test]
    async fn edit_exact_happy() {
        let (dir, tb) = setup();
        let path = write(&dir, "f.rs", "fn main() {\n    let x = 1;\n}\n");
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({"path": "f.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("(exact)"));
        let disk = std::fs::read_to_string(path).unwrap();
        assert_eq!(disk, "fn main() {\n    let x = 2;\n}\n");
    }

    #[tokio::test]
    async fn edit_ladder_line_trimmed() {
        let (dir, tb) = setup();
        let path = write(&dir, "f.rs", "fn main() {\n    let x = 1;\n}\n");
        // Needle has wrong leading indent → exact fails, (b) matches.
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({"path": "f.rs", "old_string": "  let x = 1;\n  }", "new_string": "    let x = 9;\n}"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("line-trimmed"));
        let disk = std::fs::read_to_string(path).unwrap();
        assert_eq!(disk, "fn main() {\n    let x = 9;\n}\n");
    }

    #[tokio::test]
    async fn edit_ladder_whitespace_normalized() {
        let (dir, tb) = setup();
        // Internal whitespace runs differ → (b) fails, (c) matches.
        let path = write(&dir, "f.rs", "let  y  =\t2;\n");
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({"path": "f.rs", "old_string": "let y = 2;", "new_string": "let y = 3;"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("whitespace-normalized"));
        let disk = std::fs::read_to_string(path).unwrap();
        assert_eq!(disk, "let y = 3;\n");
    }

    #[tokio::test]
    async fn edit_ladder_indentation_flexible() {
        let (dir, tb) = setup();
        // Two windows identical when trimmed → (b)/(c) ambiguous.
        // Relative indentation disambiguates → (d) matches the first block
        // and re-indents new_string to the matched block's indent.
        let content =
            "mod a {\n    if x {\n        go();\n    }\n}\nmod b {\nif x {\ngo();\n}\n}\n";
        let path = write(&dir, "f.rs", content);
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({
                    "path": "f.rs",
                    "old_string": "if x {\n    go();\n}",
                    "new_string": "if x {\n    stop();\n}"
                }),
            ))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("indentation-flexible"));
        let disk = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            disk,
            "mod a {\n    if x {\n        stop();\n    }\n}\nmod b {\nif x {\ngo();\n}\n}\n"
        );
    }

    #[tokio::test]
    async fn edit_escape_drift_refused() {
        let (dir, tb) = setup();
        write(&dir, "f.js", "    call(\"q\");\n");
        // Trailing-space needle forces a fuzzy match; new_string carrying \" → refuse.
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({
                    "path": "f.js",
                    "old_string": "call(\"q\");   ",
                    "new_string": "call(\\\"q\\\");"
                }),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("escap"), "{}", out.output);
    }

    #[tokio::test]
    async fn edit_replace_all_fuzzy_refused() {
        let (dir, tb) = setup();
        write(&dir, "f.rs", "    let x = 1;\n");
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({
                    "path": "f.rs",
                    "old_string": "let x = 1;  ",
                    "new_string": "let x = 2;",
                    "replace_all": true
                }),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("refused"), "{}", out.output);
    }

    #[tokio::test]
    async fn edit_multiple_exact_matches_lists_lines() {
        let (dir, tb) = setup();
        write(&dir, "f.txt", "foo\nbar\nfoo\n");
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({"path": "f.txt", "old_string": "foo", "new_string": "baz"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("lines 1, 3"), "{}", out.output);
    }

    #[tokio::test]
    async fn edit_already_applied() {
        let (dir, tb) = setup();
        write(&dir, "f.rs", "let x = 2;\n");
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({"path": "f.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("already applied"));
    }

    #[tokio::test]
    async fn edit_near_miss_hint_on_no_match() {
        let (dir, tb) = setup();
        write(&dir, "f.rs", "fn main() {\n    let z = 9;\n}\n");
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({"path": "f.rs", "old_string": "let zz = 99;", "new_string": "let zz = 0;"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("closest lines"), "{}", out.output);
        assert!(out.output.contains('·'), "{}", out.output);
    }

    #[tokio::test]
    async fn bash_happy() {
        let (_dir, tb) = setup();
        let out = tb
            .dispatch(&call("bash", json!({"command": "echo forged"})))
            .await;
        assert!(!out.is_error);
        assert!(out.output.contains("forged"));
    }

    #[tokio::test]
    async fn bash_timeout_kills() {
        let (_dir, tb) = setup();
        let out = tb
            .dispatch(&call("bash", json!({"command": "sleep 5", "timeout": 1})))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("timed out"), "{}", out.output);
    }

    #[tokio::test]
    async fn grep_happy() {
        let (dir, tb) = setup();
        write(&dir, "hay.txt", "one\nneedle_here\nthree\n");
        let out = tb
            .dispatch(&call("grep", json!({"pattern": "needle_here"})))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("needle_here"), "{}", out.output);
    }

    #[tokio::test]
    async fn finish_returns_summary() {
        let (_dir, tb) = setup();
        let out = tb
            .dispatch(&call("finish", json!({"summary": "all ingots forged"})))
            .await;
        assert!(!out.is_error);
        assert_eq!(out.output, "all ingots forged");
    }

    #[tokio::test]
    async fn sandbox_rejects_escape() {
        let (_dir, tb) = setup();
        for path in ["../outside.txt", "/etc/hosts", "a/../../outside.txt"] {
            let out = tb.dispatch(&call("read_file", json!({"path": path}))).await;
            assert!(out.is_error, "path {path} should be rejected");
            assert!(out.output.contains("escapes workspace"), "{}", out.output);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_rejects_symlink_escape() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let (dir, tb) = setup();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        // Read through the symlinked dir must be rejected.
        let out = tb
            .dispatch(&call("read_file", json!({"path": "link/secret.txt"})))
            .await;
        assert!(out.is_error, "read through symlink allowed: {}", out.output);
        assert!(out.output.contains("escapes workspace"), "{}", out.output);

        // Write through the symlinked dir must be rejected too.
        let out = tb
            .dispatch(&call(
                "write_file",
                json!({"path": "link/planted.txt", "content": "x"}),
            ))
            .await;
        assert!(out.is_error, "write through symlink allowed: {}", out.output);
        assert!(!outside.path().join("planted.txt").exists());

        // A file-level symlink pointing outside is rejected as well.
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("alias.txt"),
        )
        .unwrap();
        let out = tb
            .dispatch(&call("read_file", json!({"path": "alias.txt"})))
            .await;
        assert!(out.is_error, "{}", out.output);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_allows_internal_symlink() {
        let (dir, tb) = setup();
        write(&dir, "real.txt", "content\n");
        std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("ln.txt"))
            .unwrap();
        let out = tb.dispatch(&call("read_file", json!({"path": "ln.txt"}))).await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("content"));
    }

    #[tokio::test]
    async fn edit_fuzzy_preserves_crlf_line_endings() {
        let (dir, tb) = setup();
        let path = write(&dir, "f.txt", "alpha\r\nbeta\r\ngamma\r\n");
        // Trailing-space needle forces the fuzzy (line-trimmed) branch.
        let out = tb
            .dispatch(&call(
                "edit_file",
                json!({"path": "f.txt", "old_string": "beta   ", "new_string": "BETA"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.output);
        let disk = std::fs::read_to_string(path).unwrap();
        assert_eq!(disk, "alpha\r\nBETA\r\ngamma\r\n");
    }

    #[tokio::test]
    async fn unknown_tool_and_bad_json_are_errors() {
        let (_dir, tb) = setup();
        let out = tb.dispatch(&call("teleport", json!({}))).await;
        assert!(out.is_error);
        assert!(out.output.contains("unknown tool"));

        let bad = ToolCall {
            id: "t2".into(),
            name: "read_file".into(),
            arguments: "{not json".into(),
        };
        let out = tb.dispatch(&bad).await;
        assert!(out.is_error);
        assert!(out.output.contains("bad JSON"));
    }

    #[test]
    fn path_access_classifies_tools() {
        let c = call("read_file", json!({"path": "a.txt"}));
        assert_eq!(ToolBox::path_access(&c), Some(("a.txt".into(), false)));
        let c = call("write_file", json!({"path": "b.txt", "content": "x"}));
        assert_eq!(ToolBox::path_access(&c), Some(("b.txt".into(), true)));
        let c = call(
            "edit_file",
            json!({"path": "c.txt", "old_string": "a", "new_string": "b"}),
        );
        assert_eq!(ToolBox::path_access(&c), Some(("c.txt".into(), true)));
        for name in ["bash", "grep", "recipe_view", "finish"] {
            let c = call(
                name,
                json!({"command": "x", "pattern": "y", "summary": "z", "path": "p", "name": "n"}),
            );
            assert_eq!(ToolBox::path_access(&c), None);
        }
    }

    #[test]
    fn specs_cover_seven_tools() {
        let names: Vec<String> = ToolBox::specs().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "read_file",
                "write_file",
                "edit_file",
                "bash",
                "grep",
                "recipe_view",
                "finish"
            ]
        );
    }

    fn write_recipe(dir: &tempfile::TempDir, sub: &str, frontmatter: &str, body: &str) {
        let d = dir.path().join("recipes").join(sub);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("RECIPE.md"), format!("---\n{frontmatter}\n---\n{body}")).unwrap();
    }

    #[tokio::test]
    async fn recipe_view_happy() {
        let (dir, tb) = setup();
        write_recipe(
            &dir,
            "ship",
            "name: ship\ndescription: deploy the site",
            "Step 1: build.\nStep 2: deploy.\n",
        );
        let out = tb
            .dispatch(&call("recipe_view", json!({"name": "ship"})))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("Step 1: build."), "{}", out.output);
        assert!(out.output.contains("name: ship"), "full RECIPE.md expected");
    }

    #[tokio::test]
    async fn recipe_view_unknown_lists_known_names() {
        let (dir, tb) = setup();
        write_recipe(&dir, "ship", "name: ship\ndescription: deploy", "Body\n");
        let out = tb
            .dispatch(&call("recipe_view", json!({"name": "ghost"})))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("unknown recipe 'ghost'"), "{}", out.output);
        assert!(out.output.contains("ship"), "{}", out.output);
    }

    #[tokio::test]
    async fn recipe_view_refuses_bare_name_on_collision() {
        let (dir, tb) = setup();
        // Two workspace dirs whose frontmatter claims the same name.
        write_recipe(&dir, "a", "name: dup\ndescription: first", "A\n");
        write_recipe(&dir, "b", "name: dup\ndescription: second", "B\n");
        let out = tb
            .dispatch(&call("recipe_view", json!({"name": "dup"})))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("name collision"), "{}", out.output);
    }
}
