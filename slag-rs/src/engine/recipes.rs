//! recipes — the hermes skills system, metallurgy-named.
//!
//! A recipe is `recipes/<name>/RECIPE.md` with `---` fenced frontmatter
//! (name, description, optional requires_tools). Two roots: the workspace
//! (`<root>/recipes/`) and the config dir (`$SLAG_CONFIG_DIR` or
//! `~/.config/slag/recipes/`). Name collisions are loud: the index marks
//! both entries and `recipe_view` refuses the bare name. The rendered index
//! is snapshot-cached in the config dir, keyed on an mtime+size manifest.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

/// One discovered recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    /// Path to the RECIPE.md file.
    pub path: PathBuf,
    /// Tool names this recipe needs; hidden from the index when any is missing.
    pub requires_tools: Vec<String>,
    /// Tools the recipe wants the session limited to (item 98). Advisory:
    /// surfaced by recipe_view; per-recipe enforcement awaits recipe-bound
    /// sessions.
    pub allowed_tools: Vec<String>,
    /// Model the recipe prefers to run on (advisory, surfaced).
    pub model: Option<String>,
    /// Where the recipe should run: inline in the current session
    /// (default) or forked into a sub-smith.
    pub context: RecipeContext,
    /// Path globs gating index visibility (item 98): a recipe naming
    /// `paths` is hidden until the workspace contains a matching file,
    /// cutting index tokens for stacks the project does not use.
    pub paths: Vec<String>,
}

/// Recipe execution context (item 98). Fork execution is not wired yet
/// (NativeSmith's recipe binding is still reserved); the value parses and
/// surfaces so recipes can declare intent today.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecipeContext {
    #[default]
    Inline,
    Fork,
}

/// Discover all recipes: workspace first, then config dir, each sorted by
/// name. On a name collision both entries are kept (loud-collision pattern);
/// the workspace entry sorts first within its group.
pub fn discover(root: &Path) -> Vec<Recipe> {
    discover_in(root, config_dir().as_deref())
}

/// Render the recipes index for the volatile prompt band.
/// Recipes whose `requires_tools` are not all in `available_tools` are
/// hidden. Colliding names are listed with a `[name collision]` marker.
/// Reuses the snapshot cache when the on-disk manifest is unchanged.
pub fn index(root: &Path, available_tools: &[String]) -> String {
    index_in(root, config_dir().as_deref(), available_tools)
}

/// Config directory: $SLAG_CONFIG_DIR override, else ~/.config/slag.
/// Mirrors config.rs resolution; implemented locally to keep engine
/// submodules self-contained.
fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SLAG_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("slag"))
}

fn discover_in(root: &Path, config_dir: Option<&Path>) -> Vec<Recipe> {
    let mut out = Vec::new();
    collect(&root.join("recipes"), &mut out);
    if let Some(cd) = config_dir {
        collect(&cd.join("recipes"), &mut out);
    }
    out
}

fn collect(dir: &Path, out: &mut Vec<Recipe>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut batch = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("RECIPE.md");
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let fm = parse_frontmatter(&content);
        let name = fm
            .name
            .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
        batch.push(Recipe {
            name,
            description: fm.description,
            path,
            requires_tools: fm.requires_tools,
            allowed_tools: fm.allowed_tools,
            model: fm.model,
            context: fm.context,
            paths: fm.paths,
        });
    }
    batch.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(batch);
}

#[derive(Debug, Default)]
struct Frontmatter {
    name: Option<String>,
    description: String,
    requires_tools: Vec<String>,
    allowed_tools: Vec<String>,
    model: Option<String>,
    context: RecipeContext,
    paths: Vec<String>,
}

/// Which key a dash list under a bare `key:` line continues.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKey {
    None,
    Requires,
    Allowed,
    Paths,
}

/// Hand-rolled YAML-ish frontmatter parse: `---` fenced block at the top,
/// `key: value` lines. The list keys (`requires_tools`, `allowed_tools`,
/// `paths`) each accept a comma list, an inline `[a, b]` list, or a dash
/// list on following lines. No yaml dependency.
fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut lines = content.lines().peekable();
    while matches!(lines.peek(), Some(l) if l.trim().is_empty()) {
        lines.next();
    }
    if lines.next().map(str::trim) != Some("---") {
        return fm;
    }
    let mut open_list = ListKey::None;
    for line in lines {
        let t = line.trim();
        if t == "---" {
            break;
        }
        if open_list != ListKey::None {
            if let Some(item) = t.strip_prefix('-') {
                let item = item.trim().trim_matches(|c| c == '"' || c == '\'');
                if !item.is_empty() {
                    list_bucket(&mut fm, open_list).push(item.to_string());
                }
                continue;
            }
            open_list = ListKey::None;
        }
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let list_key = match key {
            "requires_tools" => ListKey::Requires,
            "allowed_tools" => ListKey::Allowed,
            "paths" => ListKey::Paths,
            _ => ListKey::None,
        };
        if list_key != ListKey::None {
            if value.is_empty() {
                open_list = list_key;
            } else {
                *list_bucket(&mut fm, list_key) = parse_inline_list(value);
            }
            continue;
        }
        match key {
            "name" if !value.is_empty() => fm.name = Some(value.to_string()),
            "description" => fm.description = value.to_string(),
            "model" if !value.is_empty() => fm.model = Some(value.to_string()),
            "context" => {
                if value.eq_ignore_ascii_case("fork") {
                    fm.context = RecipeContext::Fork;
                }
            }
            _ => {}
        }
    }
    fm
}

fn list_bucket(fm: &mut Frontmatter, key: ListKey) -> &mut Vec<String> {
    match key {
        ListKey::Requires | ListKey::None => &mut fm.requires_tools,
        ListKey::Allowed => &mut fm.allowed_tools,
        ListKey::Paths => &mut fm.paths,
    }
}

/// `[a, b]` or `a, b` → items.
fn parse_inline_list(value: &str) -> Vec<String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    inner
        .split(',')
        .map(|item| item.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Prompt-band budget: the index never grows past these caps, however
/// many (or however verbose) the installed recipes are.
const INDEX_MAX_RECIPES: usize = 50;
const INDEX_MAX_DESC_CHARS: usize = 200;

fn render_index(recipes: &[Recipe], available_tools: &[String], root: &Path) -> String {
    let mut lines = Vec::new();
    for r in recipes {
        if lines.len() >= INDEX_MAX_RECIPES {
            break;
        }
        if !r
            .requires_tools
            .iter()
            .all(|t| available_tools.iter().any(|a| a == t))
        {
            continue;
        }
        // Paths gating (item 98): hidden until the workspace holds a
        // matching file.
        if !r.paths.is_empty() && !paths_match_workspace(root, &r.paths) {
            continue;
        }
        let collides = recipes.iter().filter(|o| o.name == r.name).count() > 1;
        let marker = if collides { " [name collision]" } else { "" };
        let desc = if r.description.chars().count() > INDEX_MAX_DESC_CHARS {
            let cut: String = r.description.chars().take(INDEX_MAX_DESC_CHARS).collect();
            format!("{cut}…")
        } else {
            r.description.clone()
        };
        lines.push(format!("- {}: {}{}", r.name, desc, marker));
    }
    if lines.is_empty() {
        return "## Recipes\n(none installed)".to_string();
    }
    format!("## Recipes\n{}", lines.join("\n"))
}

/// True when any file under `root` matches any of `patterns`. Bounded
/// walk: skips VCS/build dirs, depth-limited, entry-capped — the gate is
/// a heuristic for index visibility, not an exhaustive search.
fn paths_match_workspace(root: &Path, patterns: &[String]) -> bool {
    let mut budget = 2000usize;
    walk_match(root, root, patterns, 6, &mut budget)
}

fn walk_match(root: &Path, dir: &Path, patterns: &[String], depth: u8, budget: &mut usize) -> bool {
    if depth == 0 || *budget == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".venv" | "dist") {
                continue;
            }
            if walk_match(root, &path, patterns, depth - 1, budget) {
                return true;
            }
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        for pat in patterns {
            // A pattern without '/' matches the basename anywhere
            // (rg -g convention); one with '/' matches the relative path.
            let hay: &str = if pat.contains('/') { &rel } else { &name };
            if glob_match(pat, hay) {
                return true;
            }
        }
    }
    false
}

/// Minimal glob: `*` matches within a path segment, `**` crosses
/// segments, `?` matches one char. Hand-rolled, no glob dependency.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn inner(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => {
                let crosses = p.get(1) == Some(&'*');
                let rest = if crosses { &p[2..] } else { &p[1..] };
                // `**/` also matches zero directories.
                let rest = if crosses && rest.first() == Some(&'/') && inner(&rest[1..], t) {
                    return true;
                } else {
                    rest
                };
                for i in 0..=t.len() {
                    if inner(rest, &t[i..]) {
                        return true;
                    }
                    if i < t.len() && !crosses && t[i] == '/' {
                        return false;
                    }
                }
                false
            }
            Some('?') => !t.is_empty() && t[0] != '/' && inner(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && inner(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    inner(&p, &t)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestEntry {
    path: String,
    mtime_ns: u64,
    size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    tools: Vec<String>,
    manifest: Vec<ManifestEntry>,
    index: String,
}

const SNAPSHOT_FILE: &str = ".recipes_snapshot.json";

/// Stat every RECIPE.md under both roots. No file contents are read here —
/// that is the point of the snapshot: an unchanged manifest skips parsing.
fn build_manifest(root: &Path, config_dir: Option<&Path>) -> Vec<ManifestEntry> {
    let mut files = Vec::new();
    list_recipe_files(&root.join("recipes"), &mut files);
    if let Some(cd) = config_dir {
        list_recipe_files(&cd.join("recipes"), &mut files);
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let md = std::fs::metadata(&p).ok()?;
            let mtime_ns = md
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos() as u64;
            Some(ManifestEntry {
                path: p.display().to_string(),
                mtime_ns,
                size: md.len(),
            })
        })
        .collect()
}

fn list_recipe_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("RECIPE.md");
        if std::fs::symlink_metadata(&path).is_ok() {
            out.push(path);
        }
    }
}

fn index_in(root: &Path, config_dir: Option<&Path>, available_tools: &[String]) -> String {
    let manifest = build_manifest(root, config_dir);
    let snap_path = config_dir.map(|d| d.join(SNAPSHOT_FILE));

    let recipes = discover_in(root, config_dir);
    // A paths-gated recipe's visibility depends on workspace files the
    // manifest does not cover, so the snapshot cannot be trusted; recompute
    // every call while any such recipe exists (rare, and gating exists to
    // shrink the index anyway).
    let paths_gated = recipes.iter().any(|r| !r.paths.is_empty());

    if !paths_gated {
        if let Some(sp) = &snap_path {
            if let Some(snap) = read_snapshot(sp) {
                if snap.manifest == manifest && snap.tools.as_slice() == available_tools {
                    return snap.index;
                }
            }
        }
    }

    let index = render_index(&recipes, available_tools, root);

    // Best-effort cache write; skipped when no recipes exist at all so an
    // empty run never plants a cache file, and when paths gating makes the
    // cached string untrustworthy.
    if let Some(sp) = &snap_path {
        if !manifest.is_empty() && !paths_gated {
            let snap = Snapshot {
                tools: available_tools.to_vec(),
                manifest,
                index: index.clone(),
            };
            if let Ok(json) = serde_json::to_string(&snap) {
                if let Some(parent) = sp.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(sp, json);
            }
        }
    }

    index
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Substitute invocation arguments into a recipe body before injection.
/// Turns static recipes into parameterized ones.
///
/// `raw_args` is the argument string after the recipe name (e.g. everything
/// past `--` in `slag recipe run <name> -- args`), tokenized with shell
/// quoting rules (`shell-words`); malformed quoting falls back to a plain
/// whitespace split. Tokens shaped `name=value` become named args; the rest
/// are positional.
///
/// Placeholders (both `$NAME` and `${NAME}` spellings):
/// - `$ARGUMENTS` — all positional args joined by single spaces
/// - `$0`..`$n`   — positional args by index; out of range → empty
/// - `$name`      — named args; unknown names stay literal, so shell
///   variables inside recipe bodies (`$HOME`) survive untouched
///
/// When the template contains no recognized placeholder and args were
/// given, `ARGUMENTS: ...` is appended so the invocation never drops.
pub fn substitute_args(template: &str, raw_args: &str) -> String {
    let (named, positional) = parse_invocation_args(raw_args);
    let joined = positional.join(" ");

    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut substituted = false;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        match read_placeholder(after) {
            Some((name, consumed)) => match resolve(name, &named, &positional, &joined) {
                Some(value) => {
                    out.push_str(&value);
                    substituted = true;
                    rest = &after[consumed..];
                }
                None => {
                    // Unknown name: keep the `$` and re-scan after it so a
                    // later `$` in the same run is still found.
                    out.push('$');
                    rest = after;
                }
            },
            None => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);

    let raw_trimmed = raw_args.trim();
    if !substituted && !raw_trimmed.is_empty() {
        out.push_str("\n\nARGUMENTS: ");
        out.push_str(raw_trimmed);
    }
    out
}

/// Read a placeholder name right after a `$`. Returns the name and how many
/// bytes of the input it consumed (including braces for `${name}`).
fn read_placeholder(after: &str) -> Option<(&str, usize)> {
    if let Some(inner) = after.strip_prefix('{') {
        let end = inner.find('}')?;
        let name = &inner[..end];
        if name.is_empty() || !name.bytes().all(is_word_byte) {
            return None;
        }
        return Some((name, end + 2));
    }
    let len = after.bytes().take_while(|b| is_word_byte(*b)).count();
    if len == 0 {
        return None;
    }
    Some((&after[..len], len))
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn resolve(
    name: &str,
    named: &std::collections::HashMap<String, String>,
    positional: &[String],
    joined: &str,
) -> Option<String> {
    if name == "ARGUMENTS" {
        return Some(joined.to_string());
    }
    if name.bytes().all(|b| b.is_ascii_digit()) {
        let idx: usize = name.parse().ok()?;
        return Some(positional.get(idx).cloned().unwrap_or_default());
    }
    named.get(name).cloned()
}

/// Tokenize with shell quoting; split `name=value` tokens into named args.
/// A named key must be an identifier (`[A-Za-z_][A-Za-z0-9_]*`), so `2=3`
/// or `--flag=x` stay positional.
fn parse_invocation_args(
    raw: &str,
) -> (std::collections::HashMap<String, String>, Vec<String>) {
    let tokens = shell_words::split(raw)
        .unwrap_or_else(|_| raw.split_whitespace().map(str::to_string).collect());
    let mut named = std::collections::HashMap::new();
    let mut positional = Vec::new();
    for tok in tokens {
        if let Some((key, value)) = tok.split_once('=') {
            if is_identifier(key) {
                named.insert(key.to_string(), value.to_string());
                continue;
            }
        }
        positional.push(tok);
    }
    (named, positional)
}

fn is_identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(is_word_byte)
}

// ---------------------------------------------------------------------------
// Inline shell spans (item 78)
// ---------------------------------------------------------------------------

/// Max bytes spliced per span. One `!`cat huge.log`` must not blow the
/// prompt, so anything longer keeps its head and tail with a marker.
const SPAN_OUTPUT_CAP: usize = 4096;

/// Per-span shell timeout. Expansion sits in front of a model call, so a
/// hung span would stall the whole session.
const SPAN_TIMEOUT_SECS: u64 = 30;

/// What the scanner found: either a command to run or inert text to emit.
struct Found {
    /// Offset of the opener in `body`.
    start: usize,
    /// Offset to resume scanning from — always past the whole span, so
    /// spliced output is never rescanned.
    end: usize,
    /// Command byte range, or `None` when the span is inert.
    cmd: Option<(usize, usize)>,
    /// Emitted verbatim when `cmd` is `None`.
    literal: &'static str,
    /// Inline spans drop the trailing newline so the value lands
    /// mid-sentence; fenced blocks splice verbatim.
    trim: bool,
}

/// Run the shell spans in a recipe body and splice their stdout.
///
/// Two syntaxes:
/// - inline `` !`cmd` `` anywhere in the body,
/// - a fence opened with ```` ```! ```` whose whole body is one script.
///
/// A backslash escapes the opener (`` \!` `` → literal `` !` ``), so a
/// recipe can document the feature without running it. An unterminated
/// span stays literal.
///
/// Every command goes through `ToolBox::bash`, so spans inherit the
/// destructive-command refusal and the `[policy]` deny/ask rules exactly —
/// a recipe is not a hole around the policy engine. A span that fails
/// splices a marked error instead of aborting the recipe; a non-zero exit
/// arrives as bash's own `(exit N)` note.
///
/// The output string is built by hand: literal segments and command stdout
/// are pushed once and never revisited, so output that itself contains
/// `` !`...` `` or `$0` lands inert.
pub async fn expand_shell_spans(tools: &super::ToolBox, body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut at = 0usize;
    while let Some(found) = next_span(body, at) {
        out.push_str(&body[at..found.start]);
        match found.cmd {
            Some((s, e)) => out.push_str(&run_span(tools, &body[s..e], found.trim).await),
            None => out.push_str(found.literal),
        }
        at = found.end;
    }
    out.push_str(&body[at..]);
    out
}

/// Locate the next span at or after `at`. The earliest opener wins; an
/// escape starts one byte before the inline span it hides, so it wins the
/// tie by construction.
fn next_span(body: &str, at: usize) -> Option<Found> {
    let hay = &body[at..];
    let escaped = hay.find("\\!`").map(|i| at + i);
    let inline = hay.find("!`").map(|i| at + i);
    let fence = next_fence_open(body, at);

    let start = [escaped, inline, fence].into_iter().flatten().min()?;
    if escaped == Some(start) {
        return Some(inert(start, start + 3, "!`"));
    }
    if fence == Some(start) {
        return Some(fence_span(body, start));
    }
    let open = start + 2;
    match body[open..].find('`') {
        // Unterminated: emit the opener as text and keep scanning past it.
        None => Some(inert(start, open, "!`")),
        Some(rel) => Some(Found {
            start,
            end: open + rel + 1,
            cmd: Some((open, open + rel)),
            literal: "",
            trim: true,
        }),
    }
}

fn inert(start: usize, end: usize, literal: &'static str) -> Found {
    Found { start, end, cmd: None, literal, trim: false }
}

/// Next ```` ```! ```` opener that starts a line and carries nothing else
/// on it. An info-string spelling (```` ```!foo ````) is not a fence.
fn next_fence_open(body: &str, at: usize) -> Option<usize> {
    let mut from = at;
    while let Some(rel) = body[from..].find("```!") {
        let start = from + rel;
        if at_line_start(body, start) && rest_of_line(body, start + 4).trim().is_empty() {
            return Some(start);
        }
        from = start + 4;
    }
    None
}

fn fence_span(body: &str, start: usize) -> Found {
    // The script begins on the line after the opener.
    let Some(rel) = body[start..].find('\n') else {
        return inert(start, start + 4, "```!");
    };
    let cmd_start = start + rel + 1;
    match find_fence_close(body, cmd_start) {
        Some((close, next)) => Found {
            start,
            end: next,
            cmd: Some((cmd_start, close)),
            literal: "",
            trim: false,
        },
        // No closing fence: leave the opener as text rather than running
        // the rest of the file as a script.
        None => inert(start, start + 4, "```!"),
    }
}

/// Find the closing ```` ``` ```` line at or after `from`. Returns the
/// offset of the fence and the offset just past its line.
fn find_fence_close(body: &str, from: usize) -> Option<(usize, usize)> {
    let mut at = from;
    loop {
        let pos = at + body[at..].find("```")?;
        if at_line_start(body, pos) {
            let after = pos + 3;
            let line_end = body[after..].find('\n');
            let rest = match line_end {
                Some(i) => &body[after..after + i],
                None => &body[after..],
            };
            if rest.trim().is_empty() {
                let next = line_end.map_or(body.len(), |i| after + i + 1);
                return Some((pos, next));
            }
        }
        at = pos + 3;
    }
}

fn at_line_start(body: &str, pos: usize) -> bool {
    pos == 0 || body.as_bytes()[pos - 1] == b'\n'
}

fn rest_of_line(body: &str, from: usize) -> &str {
    let tail = &body[from..];
    tail.split('\n').next().unwrap_or("")
}

/// Run one span through the shared bash gate and shape its output.
async fn run_span(tools: &super::ToolBox, cmd: &str, trim: bool) -> String {
    let command = cmd.trim();
    if command.is_empty() {
        return String::new();
    }
    let args = serde_json::json!({ "command": command, "timeout": SPAN_TIMEOUT_SECS });
    let out = match tools.bash(&args).await {
        Ok(out) => out,
        Err(e) => format!("[slag: span failed: {e}]"),
    };
    let capped = super::truncate_middle(&out, SPAN_OUTPUT_CAP);
    if trim {
        capped.trim_end_matches('\n').to_string()
    } else {
        capped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_recipe(base: &Path, dir: &str, frontmatter: &str, body: &str) -> PathBuf {
        let d = base.join("recipes").join(dir);
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("RECIPE.md");
        std::fs::write(&path, format!("---\n{frontmatter}\n---\n{body}")).unwrap();
        path
    }

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn frontmatter_parses_item_98_fields() {
        let dir = tempfile::tempdir().unwrap();
        write_recipe(
            dir.path(),
            "web",
            "name: web\ndescription: d\nallowed_tools: [read_file, bash]\nmodel: openrouter/auto\ncontext: fork\npaths:\n  - \"*.css\"\n  - src/**/*.html",
            "body",
        );
        let r = &discover_in(dir.path(), None)[0];
        assert_eq!(r.allowed_tools, tools(&["read_file", "bash"]));
        assert_eq!(r.model.as_deref(), Some("openrouter/auto"));
        assert_eq!(r.context, RecipeContext::Fork);
        assert_eq!(r.paths, tools(&["*.css", "src/**/*.html"]));
        // Absent fields keep today's defaults (backward compatible).
        write_recipe(dir.path(), "plain", "name: plain\ndescription: d", "b");
        let plain = discover_in(dir.path(), None)
            .into_iter()
            .find(|r| r.name == "plain")
            .unwrap();
        assert!(plain.allowed_tools.is_empty());
        assert!(plain.model.is_none());
        assert_eq!(plain.context, RecipeContext::Inline);
        assert!(plain.paths.is_empty());
    }

    #[test]
    fn paths_gating_hides_until_matching_files_exist() {
        let dir = tempfile::tempdir().unwrap();
        write_recipe(
            dir.path(),
            "css",
            "name: css\ndescription: styles\npaths: [\"*.css\"]",
            "b",
        );
        let recipes = discover_in(dir.path(), None);
        // No .css file anywhere: hidden.
        let idx = render_index(&recipes, &tools(&[]), dir.path());
        assert!(!idx.contains("css:"), "{idx}");
        // A matching file appears (nested): visible.
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/site.css"), "x").unwrap();
        let idx = render_index(&recipes, &tools(&[]), dir.path());
        assert!(idx.contains("css: styles"), "{idx}");
    }

    #[test]
    fn glob_match_covers_star_doublestar_and_question() {
        assert!(glob_match("*.css", "site.css"));
        assert!(!glob_match("*.css", "site.scss.map"));
        assert!(glob_match("src/**/*.html", "src/a/b/page.html"));
        assert!(glob_match("src/**/*.html", "src/page.html"));
        assert!(!glob_match("src/*.html", "src/a/page.html"));
        assert!(glob_match("f?o.rs", "foo.rs"));
        assert!(!glob_match("f?o.rs", "f/o.rs"));
    }

    #[test]
    fn frontmatter_parses_comma_list() {
        let fm = parse_frontmatter(
            "---\nname: ship\ndescription: Deploy the site\nrequires_tools: bash, grep\n---\nBody\n",
        );
        assert_eq!(fm.name.as_deref(), Some("ship"));
        assert_eq!(fm.description, "Deploy the site");
        assert_eq!(fm.requires_tools, vec!["bash", "grep"]);
    }

    #[test]
    fn frontmatter_parses_dash_list_and_inline_list() {
        let fm = parse_frontmatter(
            "---\nname: a\ndescription: d\nrequires_tools:\n  - bash\n  - edit_file\n---\n",
        );
        assert_eq!(fm.requires_tools, vec!["bash", "edit_file"]);

        let fm = parse_frontmatter("---\nname: b\nrequires_tools: [grep, bash]\n---\n");
        assert_eq!(fm.requires_tools, vec!["grep", "bash"]);
    }

    #[test]
    fn frontmatter_missing_fence_yields_defaults() {
        let fm = parse_frontmatter("# just a doc\nname: nope\n");
        assert_eq!(fm.name, None);
        assert_eq!(fm.description, "");
        assert!(fm.requires_tools.is_empty());
    }

    #[test]
    fn frontmatter_tolerates_leading_blank_lines_and_colons_in_values() {
        let fm = parse_frontmatter("\n\n---\nname: x\ndescription: use a:b syntax\n---\n");
        assert_eq!(fm.name.as_deref(), Some("x"));
        assert_eq!(fm.description, "use a:b syntax");
    }

    #[test]
    fn discovery_finds_workspace_and_config_recipes() {
        let ws = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        write_recipe(ws.path(), "zeta", "name: zeta\ndescription: from workspace", "");
        write_recipe(ws.path(), "alpha", "name: alpha\ndescription: also workspace", "");
        write_recipe(cfg.path(), "gamma", "name: gamma\ndescription: from config", "");

        let found = discover_in(ws.path(), Some(cfg.path()));
        let names: Vec<&str> = found.iter().map(|r| r.name.as_str()).collect();
        // Workspace group (sorted) first, then config group.
        assert_eq!(names, vec!["alpha", "zeta", "gamma"]);
    }

    #[test]
    fn recipe_without_name_key_falls_back_to_dir_name() {
        let ws = tempfile::tempdir().unwrap();
        write_recipe(ws.path(), "dirname", "description: no name key", "");
        let found = discover_in(ws.path(), None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "dirname");
    }

    #[test]
    fn collision_lists_both_with_marker() {
        let ws = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        write_recipe(ws.path(), "dup", "name: dup\ndescription: workspace one", "");
        write_recipe(cfg.path(), "dup", "name: dup\ndescription: config one", "");

        let found = discover_in(ws.path(), Some(cfg.path()));
        assert_eq!(found.len(), 2, "collision keeps BOTH entries");

        let idx = render_index(&found, &tools(&[]), ws.path());
        assert_eq!(idx.matches("[name collision]").count(), 2, "{idx}");
        assert!(idx.contains("- dup: workspace one [name collision]"), "{idx}");
        assert!(idx.contains("- dup: config one [name collision]"), "{idx}");
    }

    #[test]
    fn requires_tools_filter_hides_unmet_recipes() {
        let ws = tempfile::tempdir().unwrap();
        write_recipe(
            ws.path(),
            "web",
            "name: web\ndescription: needs browser\nrequires_tools: browser",
            "",
        );
        write_recipe(ws.path(), "plain", "name: plain\ndescription: no needs", "");

        let found = discover_in(ws.path(), None);
        let idx = render_index(&found, &tools(&["bash", "grep"]), ws.path());
        assert!(idx.contains("- plain: no needs"), "{idx}");
        assert!(!idx.contains("web"), "{idx}");

        let idx = render_index(&found, &tools(&["bash", "browser"]), ws.path());
        assert!(idx.contains("- web: needs browser"), "{idx}");
    }

    #[test]
    fn index_caps_recipe_count_and_description_length() {
        let dir = tempfile::tempdir().unwrap();
        let recipes: Vec<Recipe> = (0..60)
            .map(|i| Recipe {
                name: format!("r{i:02}"),
                description: "d".repeat(300),
                path: PathBuf::from("RECIPE.md"),
                requires_tools: vec![],
                allowed_tools: vec![],
                model: None,
                context: RecipeContext::Inline,
                paths: vec![],
            })
            .collect();
        let idx = render_index(&recipes, &tools(&[]), dir.path());
        let entries: Vec<&str> = idx.lines().filter(|l| l.starts_with("- ")).collect();
        assert_eq!(entries.len(), INDEX_MAX_RECIPES);
        for e in entries {
            assert!(e.ends_with('…'), "long description must be truncated: {e}");
            assert!(e.chars().count() < 300, "got {} chars", e.chars().count());
        }
    }

    #[test]
    fn index_empty_case() {
        let ws = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let idx = index_in(ws.path(), Some(cfg.path()), &tools(&["bash"]));
        assert_eq!(idx, "## Recipes\n(none installed)");
        assert!(
            !cfg.path().join(SNAPSHOT_FILE).exists(),
            "empty run must not plant a snapshot"
        );
    }

    #[test]
    fn snapshot_reused_when_unchanged_and_invalidated_on_mtime_change() {
        let ws = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let recipe = write_recipe(ws.path(), "ship", "name: ship\ndescription: deploy", "Body");
        let avail = tools(&["bash"]);

        let first = index_in(ws.path(), Some(cfg.path()), &avail);
        assert!(first.contains("- ship: deploy"));
        let snap_path = cfg.path().join(SNAPSHOT_FILE);
        assert!(snap_path.exists());

        // Tamper with the cached index; an unchanged manifest must return
        // the cached value verbatim (proves the parse was skipped).
        let mut snap = read_snapshot(&snap_path).unwrap();
        snap.index = "## Recipes\n- CACHED SENTINEL".to_string();
        std::fs::write(&snap_path, serde_json::to_string(&snap).unwrap()).unwrap();
        let second = index_in(ws.path(), Some(cfg.path()), &avail);
        assert_eq!(second, "## Recipes\n- CACHED SENTINEL");

        // Bump mtime only (same size, same content) → snapshot invalid.
        let f = std::fs::OpenOptions::new().append(true).open(&recipe).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let third = index_in(ws.path(), Some(cfg.path()), &avail);
        assert_eq!(third, first, "mtime change must force a fresh render");
    }

    #[test]
    fn snapshot_invalidated_when_available_tools_change() {
        let ws = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        write_recipe(
            ws.path(),
            "web",
            "name: web\ndescription: needs browser\nrequires_tools: browser",
            "",
        );

        let without = index_in(ws.path(), Some(cfg.path()), &tools(&["bash"]));
        assert!(!without.contains("web"));
        let with = index_in(ws.path(), Some(cfg.path()), &tools(&["bash", "browser"]));
        assert!(with.contains("- web: needs browser"), "{with}");
    }

    #[test]
    fn arguments_placeholder_joins_all_positionals() {
        let out = substitute_args("Deploy $ARGUMENTS now", "web prod");
        assert_eq!(out, "Deploy web prod now");
        let out = substitute_args("Deploy ${ARGUMENTS} now", "web prod");
        assert_eq!(out, "Deploy web prod now");
    }

    #[test]
    fn positional_placeholders_index_from_zero() {
        let out = substitute_args("first=$0 second=${1} missing=$2.", "a b");
        assert_eq!(out, "first=a second=b missing=.");
    }

    #[test]
    fn multi_digit_index_is_one_placeholder_not_index_plus_literal() {
        let args = "a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 a10";
        assert_eq!(substitute_args("got $10", args), "got a10");
    }

    #[test]
    fn named_args_substitute_and_leave_positionals_clean() {
        let out = substitute_args(
            "env=$env target=${target} rest: $ARGUMENTS",
            "env=prod target=web extra stuff",
        );
        assert_eq!(out, "env=prod target=web rest: extra stuff");
    }

    #[test]
    fn shell_quoting_keeps_spaces_inside_one_arg() {
        let out = substitute_args("msg=$0 next=$1", "\"hello world\" x");
        assert_eq!(out, "msg=hello world next=x");
    }

    #[test]
    fn malformed_quoting_falls_back_to_whitespace_split() {
        // Unterminated quote: shell-words errors; whitespace split applies.
        let out = substitute_args("$0|$1", "\"unterminated two");
        assert_eq!(out, "\"unterminated|two");
    }

    #[test]
    fn no_placeholder_appends_arguments_line() {
        let out = substitute_args("Static recipe body", "web prod");
        assert_eq!(out, "Static recipe body\n\nARGUMENTS: web prod");
    }

    #[test]
    fn no_placeholder_and_no_args_leaves_template_untouched() {
        assert_eq!(substitute_args("Static recipe body", "   "), "Static recipe body");
    }

    #[test]
    fn unknown_names_stay_literal_and_still_trigger_append() {
        // $HOME is not a recipe placeholder: it survives verbatim, and since
        // nothing substituted, the args are appended instead of dropped.
        let out = substitute_args("cd $HOME && ls", "web");
        assert_eq!(out, "cd $HOME && ls\n\nARGUMENTS: web");
    }

    #[test]
    fn dollar_edge_cases_pass_through() {
        assert_eq!(substitute_args("cost: $ or ${bad name} or $", "x"),
            "cost: $ or ${bad name} or $\n\nARGUMENTS: x");
    }

    #[test]
    fn non_identifier_equals_tokens_stay_positional() {
        let out = substitute_args("$0 and $1 and $ARGUMENTS", "2=3 --flag=x");
        assert_eq!(out, "2=3 and --flag=x and 2=3 --flag=x");
    }

    #[test]
    fn builtin_arguments_wins_over_a_named_collision() {
        let out = substitute_args("$ARGUMENTS", "ARGUMENTS=hijack real");
        assert_eq!(out, "real");
    }

    #[test]
    fn corrupt_snapshot_is_ignored() {
        let ws = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        write_recipe(ws.path(), "ship", "name: ship\ndescription: deploy", "");
        std::fs::write(cfg.path().join(SNAPSHOT_FILE), "{not json").unwrap();
        let idx = index_in(ws.path(), Some(cfg.path()), &tools(&[]));
        assert!(idx.contains("- ship: deploy"), "{idx}");
    }

    // --- item 78: inline shell spans -------------------------------------

    fn span_box(root: &Path) -> super::super::ToolBox {
        super::super::ToolBox::new(root)
    }

    #[tokio::test]
    async fn inline_span_splices_stdout() {
        let ws = tempfile::tempdir().unwrap();
        let out = expand_shell_spans(&span_box(ws.path()), "before !`echo hi` after").await;
        assert_eq!(out, "before hi after");
    }

    #[tokio::test]
    async fn fenced_block_runs_as_one_script() {
        let ws = tempfile::tempdir().unwrap();
        let body = "head\n```!\nA=1\necho \"val=$A\"\n```\ntail";
        let out = expand_shell_spans(&span_box(ws.path()), body).await;
        assert_eq!(out, "head\nval=1\ntail");
    }

    #[tokio::test]
    async fn failing_command_surfaces_exit_code_without_aborting() {
        let ws = tempfile::tempdir().unwrap();
        let out =
            expand_shell_spans(&span_box(ws.path()), "a !`exit 3` b !`echo ok` c").await;
        assert!(out.contains("(exit 3)"), "{out}");
        // The rest of the recipe still expands.
        assert!(out.starts_with("a "), "{out}");
        assert!(out.contains("ok"), "{out}");
        assert!(out.ends_with(" c"), "{out}");
    }

    #[tokio::test]
    async fn command_output_containing_a_span_lands_inert() {
        let ws = tempfile::tempdir().unwrap();
        // stdout carries span-looking text and a placeholder; neither may
        // re-expand — the scanner never revisits what it has emitted.
        // `\140` is the backtick, so the command itself holds none.
        let out = expand_shell_spans(
            &span_box(ws.path()),
            "x !`printf '!\\140echo PWNED\\140 $0'` y",
        )
        .await;
        assert_eq!(out, "x !`echo PWNED` $0 y");
    }

    #[tokio::test]
    async fn backslash_escapes_a_literal_span() {
        let ws = tempfile::tempdir().unwrap();
        let out =
            expand_shell_spans(&span_box(ws.path()), "write \\!`echo hi` to run one").await;
        assert_eq!(out, "write !`echo hi` to run one");
    }

    #[tokio::test]
    async fn unterminated_span_stays_literal() {
        let ws = tempfile::tempdir().unwrap();
        let out = expand_shell_spans(&span_box(ws.path()), "oops !`echo hi").await;
        assert_eq!(out, "oops !`echo hi");
        // A bare `!` is untouched too.
        let plain = expand_shell_spans(&span_box(ws.path()), "hi! there").await;
        assert_eq!(plain, "hi! there");
    }

    #[tokio::test]
    async fn oversized_span_output_is_capped() {
        let ws = tempfile::tempdir().unwrap();
        let out = expand_shell_spans(
            &span_box(ws.path()),
            "start !`printf 'x%.0s' $(seq 1 20000)` end",
        )
        .await;
        assert!(out.len() < SPAN_OUTPUT_CAP + 200, "len {}", out.len());
        assert!(out.contains("truncated"), "{out}");
        assert!(out.starts_with("start "), "{out}");
        assert!(out.ends_with(" end"), "{out}");
    }

    #[tokio::test]
    async fn refused_span_reports_the_gate_and_keeps_going() {
        let ws = tempfile::tempdir().unwrap();
        // A `[policy]` deny rule stands in for the shared gate: the span
        // must go through ToolBox::bash, not around it. Denial happens
        // before the spawn, so nothing runs.
        let policy = super::super::super::policy::Policy::from_entries(&[(
            "deny".into(),
            "curl:*".into(),
        )]);
        let tb = span_box(ws.path()).with_policy(policy);
        let out = expand_shell_spans(&tb, "a !`curl --version` b !`echo fine` c").await;
        assert!(out.contains("[slag: span failed"), "{out}");
        assert!(out.contains("refused by policy rule `curl:*`"), "{out}");
        assert!(out.contains("fine"), "{out}");
        assert!(out.ends_with(" c"), "{out}");
    }

    #[tokio::test]
    async fn span_inherits_the_bash_only_guards() {
        let ws = tempfile::tempdir().unwrap();
        // The sleep guard lives in ToolBox::bash, never in run_shell — a
        // span that trips it proves the gated path, not a private spawn.
        let out = expand_shell_spans(&span_box(ws.path()), "a !`sleep 30` b").await;
        assert!(out.contains("[slag: span failed"), "{out}");
        assert!(out.contains("spends the ingot's clock"), "{out}");
        // Same for the destructive-command refusal.
        let sql = expand_shell_spans(
            &span_box(ws.path()),
            "!`psql -c \"DROP TABLE users\"`",
        )
        .await;
        assert!(sql.contains("refused destructive command"), "{sql}");
    }

    #[tokio::test]
    async fn body_without_spans_is_returned_unchanged() {
        let ws = tempfile::tempdir().unwrap();
        let body = "plain text\n```sh\necho not-a-span\n```\ndone";
        let out = expand_shell_spans(&span_box(ws.path()), body).await;
        assert_eq!(out, body);
    }
}
