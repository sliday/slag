//! harness: slag's editable execution harness, after Harness² (Xu et al.,
//! "Harness²: Recursive Agent Harnessing for an Open World",
//! arXiv:2610.14224, 2026).
//!
//! A frozen model works inside a harness: files a run loads around it.
//! The paper names eight components. Seven map onto surfaces slag
//! already has, and one has no slag equivalent:
//!
//! | paper | file in a harness | how slag loads it |
//! |-------|-------------------|-------------------|
//! | C1 system prompt | `systemprompt.md` | closes the stable prompt band |
//! | C2 guardrails | `guardrails.md` | the same band |
//! | C3 memory | `memory.md` | the same band |
//! | C4 tool guidance | `tool_guidance.md` | the same band |
//! | C6 skills | `recipes/<name>/RECIPE.md` | the recipe index; `recipe_view` loads one on demand |
//! | C7 scripts | `scripts/<file>`, `recipes/<name>/scripts/<file>` | named in the band; the smith runs them with `bash`, in the shell sandbox |
//! | C8 plugin | `hooks.toml` | command hooks around each tool call, in the read-only sandbox |
//! | C5 subagents | none | slag's agent loop has no delegation tool; the validator drops `sub_agents/` |
//!
//! A missing directory, or one holding only empty files, loads nothing:
//! every prompt is then byte-identical to a run without a harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod library;
pub mod prompts;
pub mod validate;

/// The always-on files in the order the band prints them, each with its
/// heading.
pub const ALWAYS_ON: [(&str, &str); 4] = [
    ("systemprompt.md", "Procedure"),
    ("guardrails.md", "Guardrails"),
    ("memory.md", "Memory"),
    ("tool_guidance.md", "Tool guidance"),
];
pub const RECIPES: &str = "recipes";
pub const SCRIPTS: &str = "scripts";
pub const HOOKS: &str = "hooks.toml";
/// The variable a harness hook reads to find the harness's own files.
pub const HARNESS_ENV: &str = "SLAG_HARNESS";

/// Bounds on what `tree` reads: a harness is a handful of small files.
const TREE_MAX_FILES: usize = 200;
const TREE_MAX_BYTES: u64 = 256 * 1024;
const TREE_MAX_DEPTH: usize = 6;

/// One harness tree, loaded.
#[derive(Debug, Clone, Default)]
pub struct Harness {
    root: PathBuf,
    texts: Vec<(&'static str, String)>,
    scripts: Vec<String>,
    recipes: bool,
    hooks: Vec<(String, String)>,
}

impl Harness {
    /// Load the tree at `dir`. A missing directory is an empty harness.
    pub fn load(dir: &Path) -> Self {
        let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let files = tree(&root);
        let texts = ALWAYS_ON
            .iter()
            .filter_map(|(file, head)| {
                let text = String::from_utf8_lossy(files.get(*file)?).trim().to_string();
                (!text.is_empty()).then_some((*head, text))
            })
            .collect();
        let scripts = files.keys().filter(|p| is_script(p)).cloned().collect();
        let recipes = files.keys().any(|p| is_recipe(p));
        let hooks = files
            .get(HOOKS)
            .map(|raw| hook_lines(&String::from_utf8_lossy(raw)))
            .unwrap_or_default();
        Self { root, texts, scripts, recipes, hooks }
    }

    /// Nothing to load: no text, no script, no recipe, no hook.
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty() && self.scripts.is_empty() && !self.recipes && self.hooks.is_empty()
    }

    /// The always-on band: where the harness lives, its four texts under
    /// their headings, then the scripts it ships. Empty when the harness
    /// has nothing the model needs to read.
    pub fn band(&self) -> String {
        if self.texts.is_empty() && self.scripts.is_empty() && !self.recipes {
            return String::new();
        }
        let mut s = format!(
            "# Harness\n\nThis run's harness lives in {}. A path such as `scripts/x.py` or \
             `recipes/<name>/scripts/x.py` in this section or in a harness recipe is relative \
             to it.\n",
            self.root.display()
        );
        for (head, text) in &self.texts {
            s.push_str(&format!("\n## {head}\n{text}\n"));
        }
        if !self.scripts.is_empty() {
            s.push_str(
                "\n## Scripts\nDeterministic helpers. Run them with bash; they run in the same \
                 sandbox as your own commands:\n",
            );
            for p in &self.scripts {
                s.push_str(&format!("- {}\n", self.root.join(p).display()));
            }
        }
        s.trim_end().to_string()
    }

    /// The directory whose `recipes/` the index should search, when this
    /// harness ships recipes.
    pub fn recipe_root(&self) -> Option<PathBuf> {
        self.recipes.then(|| self.root.clone())
    }

    /// This harness's hooks, bound to run from `cwd` in the read-only
    /// sandbox, with `SLAG_HARNESS` pointing at the harness.
    pub fn hooks(&self, cwd: &Path) -> Option<crate::engine::hooks::HookSnapshot> {
        if self.hooks.is_empty() {
            return None;
        }
        let scope = crate::engine::hooks::CommandScope {
            read_only: true,
            cwd: Some(cwd.to_path_buf()),
            env: vec![(HARNESS_ENV.to_string(), self.root.display().to_string())],
        };
        Some(crate::engine::hooks::HookSnapshot::from_entries(self.hooks.clone(), false).with_scope(scope))
    }

    /// One line per component, for a report: `systemprompt.md, recipes/x`.
    pub fn summary(&self) -> String {
        let files = tree(&self.root);
        let mut parts: Vec<String> = Vec::new();
        for (file, _) in ALWAYS_ON {
            if files.contains_key(file) {
                parts.push(file.to_string());
            }
        }
        for p in files.keys() {
            if let Some(name) = p.strip_prefix("recipes/").and_then(|r| r.strip_suffix("/RECIPE.md")) {
                parts.push(format!("recipe {name}"));
            }
        }
        for p in &self.scripts {
            parts.push(p.clone());
        }
        if !self.hooks.is_empty() {
            parts.push(format!("{} hook(s)", self.hooks.len()));
        }
        if parts.is_empty() {
            "empty".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// `scripts/<file>` or `recipes/<name>/scripts/<file>`.
pub fn is_script(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    matches!(parts.as_slice(), [SCRIPTS, _] | [RECIPES, _, SCRIPTS, _])
}

/// `recipes/<name>/RECIPE.md`.
pub fn is_recipe(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    matches!(parts.as_slice(), [RECIPES, _, "RECIPE.md"])
}

/// `event = "spec"` lines from a hooks file, in order. A `[hooks]` header
/// and comments are skipped, so a block copied from `config.toml` works.
pub fn hook_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('['))
        .filter_map(|l| {
            let (key, value) = l.split_once('=')?;
            let value = value.trim();
            let unquoted = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            Some((key.trim().to_string(), unquoted.replace("\\\"", "\"")))
        })
        .collect()
}

/// Every non-empty regular file under `dir`, by `/`-joined relative path.
/// Symlinks are skipped: a harness is plain files, and a link could point
/// a later reader anywhere.
pub fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk(dir, dir, 0, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= TREE_MAX_FILES {
            return;
        }
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() && depth < TREE_MAX_DEPTH {
            walk(root, &path, depth + 1, out);
        } else if kind.is_file() {
            let small = entry.metadata().is_ok_and(|m| m.len() > 0 && m.len() <= TREE_MAX_BYTES);
            if let (true, Ok(rel), Ok(bytes)) = (small, path.strip_prefix(root), std::fs::read(&path)) {
                let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>();
                out.insert(rel.join("/"), bytes);
            }
        }
    }
}

/// Replace whatever is at `dir` with exactly `files`. Scripts come out
/// executable.
pub fn write_tree(files: &BTreeMap<String, Vec<u8>>, dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    for (rel, bytes) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        #[cfg(unix)]
        if is_script(rel) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Copy the harness at `src` to `dst`, replacing `dst`.
pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    write_tree(&tree(src), dst)
}

/// A unified diff from one tree to another, file by file. Empty when the
/// trees hold the same files with the same bytes.
pub fn diff_trees(old: &BTreeMap<String, Vec<u8>>, new: &BTreeMap<String, Vec<u8>>) -> String {
    let mut paths: Vec<&String> = old.keys().chain(new.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut out = String::new();
    for p in paths {
        let (a, b) = (old.get(p), new.get(p));
        if a == b {
            continue;
        }
        let from = if a.is_some() { format!("a/{p}") } else { "/dev/null".to_string() };
        let to = if b.is_some() { format!("b/{p}") } else { "/dev/null".to_string() };
        let (a, b) = (
            a.map(|x| String::from_utf8_lossy(x).into_owned()).unwrap_or_default(),
            b.map(|x| String::from_utf8_lossy(x).into_owned()).unwrap_or_default(),
        );
        out.push_str(&similar::TextDiff::from_lines(&a, &b).unified_diff().header(&from, &to).to_string());
    }
    out
}

/// The components two trees differ in, by path: `systemprompt.md`,
/// `recipes/dates/RECIPE.md`.
pub fn changed_paths(old: &BTreeMap<String, Vec<u8>>, new: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut paths: Vec<String> = old.keys().chain(new.keys()).cloned().collect();
    paths.sort();
    paths.dedup();
    paths.retain(|p| old.get(p) != new.get(p));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_blank_harness_is_empty_and_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let none = Harness::load(&dir.path().join("absent"));
        assert!(none.is_empty() && none.band().is_empty() && none.recipe_root().is_none());
        std::fs::write(dir.path().join("systemprompt.md"), "  \n").unwrap();
        std::fs::write(dir.path().join("memory.md"), "").unwrap();
        let blank = Harness::load(dir.path());
        assert!(blank.is_empty(), "{blank:?}");
        assert!(blank.band().is_empty());
        assert!(blank.hooks(dir.path()).is_none());
    }

    #[test]
    fn the_band_prints_each_component_under_its_heading() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("guardrails.md"), "Never edit tests/fixtures.\n").unwrap();
        std::fs::write(root.join("systemprompt.md"), "Reproduce first.\n").unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/dates.py"), "print(1)\n").unwrap();
        let h = Harness::load(root);
        let band = h.band();
        let proc_at = band.find("## Procedure\nReproduce first.").expect(&band);
        let guard_at = band.find("## Guardrails\nNever edit tests/fixtures.").expect(&band);
        assert!(proc_at < guard_at, "the order is the component order: {band}");
        assert!(band.contains("scripts/dates.py") && band.contains("## Scripts"), "{band}");
        assert_eq!(h.summary(), "systemprompt.md, guardrails.md, scripts/dates.py");
    }

    #[test]
    fn hook_lines_read_quoted_specs_and_skip_headers() {
        let lines = hook_lines("[hooks]\n# note\npost_tool = \"name=lint matcher=edit_file cmd='sh $SLAG_HARNESS/scripts/l.sh'\"\n");
        assert_eq!(lines, vec![("post_tool".into(), "name=lint matcher=edit_file cmd='sh $SLAG_HARNESS/scripts/l.sh'".into())]);
    }

    #[test]
    fn trees_round_trip_and_diff_by_component() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let mut files = BTreeMap::new();
        files.insert("systemprompt.md".to_string(), b"one\n".to_vec());
        files.insert("recipes/x/RECIPE.md".to_string(), b"---\nname: x\n---\n".to_vec());
        write_tree(&files, &a).unwrap();
        copy_tree(&a, &b).unwrap();
        assert_eq!(tree(&b), files);
        assert_eq!(diff_trees(&tree(&a), &tree(&b)), "");
        std::fs::write(b.join("systemprompt.md"), "two\n").unwrap();
        let diff = diff_trees(&tree(&a), &tree(&b));
        assert!(diff.contains("-one") && diff.contains("+two"), "{diff}");
        assert_eq!(changed_paths(&tree(&a), &tree(&b)), vec!["systemprompt.md".to_string()]);
    }
}
