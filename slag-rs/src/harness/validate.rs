//! The harness validator. A tree that fails it never runs: the stage that
//! wrote it gets one retry with this message, then falls back to the tree
//! it started from (paper appendix D.2).
//!
//! The checks are the paper's three discarding rules, read for slag:
//! every recipe states its trigger, every script a component names ships
//! with the harness, and every script parses. Two more keep the harness
//! cheap and safe: always-on text stays under a cap, and a hook is a
//! plain synchronous command.

use std::path::Path;
use std::time::Duration;

use super::{is_recipe, is_script, tree, ALWAYS_ON, HOOKS, RECIPES, SCRIPTS};

/// Always-on text is paid for on every turn of every run. Past this, the
/// proposal must move procedure into a recipe.
pub const ALWAYS_ON_CAP: usize = 8_000;
/// A recipe's description is its trigger line: the index shows it, and
/// it must say when to load the recipe.
const TRIGGER_MIN_WORDS: usize = 4;
const SCRIPT_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Check the tree at `dir`, dropping what is off the editable surface.
/// `Ok` carries a note per dropped file; `Err` is the message the stage
/// that wrote the tree gets back.
pub async fn validate(dir: &Path) -> Result<Vec<String>, String> {
    let notes = prune(dir);
    let files = tree(dir);
    let mut errors = Vec::new();

    let always_on: usize = ALWAYS_ON.iter().filter_map(|(f, _)| files.get(*f)).map(Vec::len).sum();
    if always_on > ALWAYS_ON_CAP {
        errors.push(format!(
            "the always-on files hold {always_on} bytes; the cap is {ALWAYS_ON_CAP}. Cut them, \
             or move a procedure into a recipe"
        ));
    }

    let recipe_dirs: std::collections::BTreeSet<&str> = files
        .keys()
        .filter_map(|p| p.strip_prefix("recipes/")?.split('/').next())
        .collect();
    let found = crate::engine::tools::recipes::collect_dir(&dir.join(RECIPES));
    for name in &recipe_dirs {
        let Some(recipe) = found.iter().find(|r| r.path.parent().and_then(|p| p.file_name()) == Some(std::ffi::OsStr::new(name))) else {
            errors.push(format!("recipes/{name}/ has no RECIPE.md"));
            continue;
        };
        if recipe.name != *name {
            errors.push(format!(
                "recipes/{name}/RECIPE.md names itself `{}`; its `name:` must match its directory",
                recipe.name
            ));
        }
        if recipe.description.split_whitespace().count() < TRIGGER_MIN_WORDS {
            errors.push(format!(
                "recipe `{name}` has no trigger: its frontmatter needs a `description:` line (a key, \
                 not a comment) that says when to load it, in at least {TRIGGER_MIN_WORDS} words, \
                 such as `description: when <the situation>, load this to <what it does>`"
            ));
        }
    }

    let reference = regex::Regex::new(r"(?:recipes/([A-Za-z0-9_-]+)/)?scripts/([A-Za-z0-9_.-]*[A-Za-z0-9_-])").unwrap();
    for (path, bytes) in &files {
        let named_by = ALWAYS_ON.iter().any(|(f, _)| f == path) || is_recipe(path) || path == HOOKS;
        if !named_by {
            continue;
        }
        let text = String::from_utf8_lossy(bytes);
        let own = path.strip_prefix("recipes/").and_then(|r| r.split('/').next());
        for cap in reference.captures_iter(&text) {
            let file = &cap[2];
            let options: Vec<String> = match (cap.get(1), own) {
                (Some(r), _) => vec![format!("recipes/{}/scripts/{file}", r.as_str())],
                (None, Some(r)) => vec![format!("recipes/{r}/scripts/{file}"), format!("scripts/{file}")],
                (None, None) => vec![format!("scripts/{file}")],
            };
            if !options.iter().any(|o| files.contains_key(o)) {
                errors.push(format!("{path} names `{}`, and the harness does not ship it", &cap[0]));
            }
        }
    }

    for path in files.keys().filter(|p| is_script(p)) {
        if let Err(why) = check_script(dir, path, &files[path]).await {
            errors.push(why);
        }
    }

    if let Some(raw) = files.get(HOOKS) {
        let lines = super::hook_lines(&String::from_utf8_lossy(raw));
        if lines.is_empty() {
            errors.push(format!("{HOOKS} holds no `event = \"spec\"` line"));
        }
        for (i, (event, spec)) in lines.iter().enumerate() {
            match crate::engine::hooks::HookDef::parse(event, spec, i) {
                None => errors.push(format!("{HOOKS}: `{event} = \"{spec}\"` is not a hook slag can run")),
                Some(def) => {
                    if !matches!(def.kind, crate::engine::hooks::HookKind::Command(_)) {
                        errors.push(format!("{HOOKS}: hook `{}` must be a `cmd=` command", def.name));
                    }
                    if def.once || def.run_async {
                        errors.push(format!("{HOOKS}: hook `{}` must not be `once` or `async`", def.name));
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(notes)
    } else {
        Err(format!("the harness failed validation:\n- {}", errors.join("\n- ")))
    }
}

/// Remove empty files (an emptied file is a removed component) and files
/// off the editable surface, then empty directories. One note per file
/// that held content and was dropped.
fn prune(dir: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut files = Vec::new();
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(_) => files.push(path),
                Err(_) => {}
            }
        }
    }
    for path in files {
        let Ok(rel) = path.strip_prefix(dir) else { continue };
        let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        let meta = std::fs::symlink_metadata(&path);
        let empty = meta.as_ref().is_ok_and(|m| m.is_file() && m.len() == 0);
        if meta.as_ref().is_ok_and(|m| m.is_file()) && !empty && on_surface(&rel) {
            continue;
        }
        let _ = std::fs::remove_file(&path);
        if empty {
            continue;
        }
        notes.push(if rel.starts_with("sub_agents/") {
            format!("dropped {rel}: slag has no subagent tool, so the harness cannot carry one")
        } else {
            format!("dropped {rel}: it is not on the harness's editable surface")
        });
    }
    remove_empty_dirs(dir);
    notes
}

fn remove_empty_dirs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            remove_empty_dirs(&entry.path());
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

fn on_surface(rel: &str) -> bool {
    let name_ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let file_ok = |s: &str| {
        !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.as_slice() {
        [f] => ALWAYS_ON.iter().any(|(a, _)| a == f) || *f == HOOKS,
        [SCRIPTS, f] => file_ok(f),
        [RECIPES, n, "RECIPE.md"] => name_ok(n),
        [RECIPES, n, SCRIPTS, f] => name_ok(n) && file_ok(f),
        _ => false,
    }
}

/// Does the script parse? Checked by its interpreter, in the sandbox, so a
/// check never runs the script and never writes. An interpreter this
/// machine lacks is not the harness's fault: the check passes.
async fn check_script(dir: &Path, rel: &str, bytes: &[u8]) -> Result<(), String> {
    let path = dir.join(rel);
    let (program, args): (&str, Vec<String>) = match rel.rsplit('.').next() {
        Some("py") => (
            "python3",
            vec![
                "-c".into(),
                "import ast,sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])".into(),
                path.display().to_string(),
            ],
        ),
        Some("sh") | Some("bash") => ("bash", vec!["-n".into(), path.display().to_string()]),
        Some("js") | Some("mjs") | Some("cjs") => ("node", vec!["--check".into(), path.display().to_string()]),
        _ if bytes.starts_with(b"#!") => return Ok(()),
        _ => {
            return Err(format!(
                "{rel} has no extension slag can check (.py, .sh, .js) and no #! line"
            ))
        }
    };
    let sandbox = crate::engine::sandbox::Sandbox::read_only()
        .unwrap_or_else(|_| crate::engine::sandbox::Sandbox::for_root(dir));
    let mut cmd = sandbox.wrap(program);
    cmd.args(&args).current_dir(dir).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = match tokio::time::timeout(SCRIPT_CHECK_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(_)) => return Ok(()),
        Err(_) => return Err(format!("{rel}: the syntax check timed out")),
    };
    match out.status.code() {
        Some(0) | Some(127) => Ok(()),
        _ => {
            let mut why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if why.is_empty() {
                why = String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
            let why: String = why.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" / ");
            Err(format!("{rel} does not parse: {why}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const RECIPE: &str = "---\nname: dates\ndescription: When a fix touches date ranges, load this to check both ends.\n---\nRun scripts/edges.py first.\n";

    #[tokio::test]
    async fn a_sound_tree_passes_and_off_surface_files_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "systemprompt.md", "When a fix touches dates, load the `dates` recipe.\n");
        put(root, "recipes/dates/RECIPE.md", RECIPE);
        put(root, "recipes/dates/scripts/edges.py", "print('ok')\n");
        put(root, "sub_agents/scout/AGENT.md", "---\nmode: subagent\n---\n");
        put(root, "notes.txt", "scratch\n");
        put(root, "memory.md", "");
        let notes = validate(root).await.expect("a sound tree passes");
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("sub_agents/scout/AGENT.md") && n.contains("no subagent tool")));
        assert!(!root.join("notes.txt").exists() && !root.join("memory.md").exists());
        assert!(!root.join("sub_agents").exists(), "empty directories go too");
        assert!(root.join("recipes/dates/scripts/edges.py").exists());
    }

    #[tokio::test]
    async fn a_recipe_without_a_trigger_fails() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "recipes/dates/RECIPE.md", "---\nname: dates\n---\nbody\n");
        let err = validate(dir.path()).await.unwrap_err();
        assert!(err.contains("recipe `dates` has no trigger"), "{err}");
        put(dir.path(), "recipes/dates/RECIPE.md", "---\nname: other\ndescription: load this when dates break across months\n---\n");
        let err = validate(dir.path()).await.unwrap_err();
        assert!(err.contains("must match its directory"), "{err}");
    }

    #[tokio::test]
    async fn a_named_script_must_ship_and_parse() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "recipes/dates/RECIPE.md", RECIPE);
        let err = validate(dir.path()).await.unwrap_err();
        assert!(err.contains("names `scripts/edges.py`"), "{err}");
        put(dir.path(), "recipes/dates/scripts/edges.py", "def broken(:\n");
        let err = validate(dir.path()).await.unwrap_err();
        assert!(err.contains("edges.py does not parse"), "{err}");
        put(dir.path(), "recipes/dates/scripts/edges.py", "print('fine')\n");
        assert!(validate(dir.path()).await.is_ok());
    }

    #[tokio::test]
    async fn hooks_must_be_plain_commands_and_text_must_stay_small() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "hooks.toml", "post_tool = \"name=ping url=https://example.com\"\n");
        let err = validate(dir.path()).await.unwrap_err();
        assert!(err.contains("must be a `cmd=` command"), "{err}");
        put(dir.path(), "hooks.toml", "post_tool = \"name=lint matcher=edit_file cmd='true'\"\n");
        assert!(validate(dir.path()).await.is_ok());
        put(dir.path(), "guardrails.md", &"x".repeat(ALWAYS_ON_CAP + 1));
        let err = validate(dir.path()).await.unwrap_err();
        assert!(err.contains("the cap is 8000"), "{err}");
    }
}
