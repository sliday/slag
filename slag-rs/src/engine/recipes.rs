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
}

/// Hand-rolled YAML-ish frontmatter parse: `---` fenced block at the top,
/// `key: value` lines. `requires_tools` accepts a comma list, an inline
/// `[a, b]` list, or a dash list on following lines. No yaml dependency.
fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut lines = content.lines().peekable();
    while matches!(lines.peek(), Some(l) if l.trim().is_empty()) {
        lines.next();
    }
    if lines.next().map(str::trim) != Some("---") {
        return fm;
    }
    let mut in_tools_list = false;
    for line in lines {
        let t = line.trim();
        if t == "---" {
            break;
        }
        if in_tools_list {
            if let Some(item) = t.strip_prefix('-') {
                let item = item.trim();
                if !item.is_empty() {
                    fm.requires_tools.push(item.to_string());
                }
                continue;
            }
            in_tools_list = false;
        }
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "name" if !value.is_empty() => fm.name = Some(value.to_string()),
            "description" => fm.description = value.to_string(),
            "requires_tools" => {
                if value.is_empty() {
                    in_tools_list = true;
                } else {
                    let inner = value
                        .strip_prefix('[')
                        .and_then(|v| v.strip_suffix(']'))
                        .unwrap_or(value);
                    fm.requires_tools = inner
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            _ => {}
        }
    }
    fm
}

/// Prompt-band budget: the index never grows past these caps, however
/// many (or however verbose) the installed recipes are.
const INDEX_MAX_RECIPES: usize = 50;
const INDEX_MAX_DESC_CHARS: usize = 200;

fn render_index(recipes: &[Recipe], available_tools: &[String]) -> String {
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

    if let Some(sp) = &snap_path {
        if let Some(snap) = read_snapshot(sp) {
            if snap.manifest == manifest && snap.tools.as_slice() == available_tools {
                return snap.index;
            }
        }
    }

    let recipes = discover_in(root, config_dir);
    let index = render_index(&recipes, available_tools);

    // Best-effort cache write; skipped when no recipes exist at all so an
    // empty run never plants a cache file.
    if let Some(sp) = &snap_path {
        if !manifest.is_empty() {
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

        let idx = render_index(&found, &tools(&[]));
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
        let idx = render_index(&found, &tools(&["bash", "grep"]));
        assert!(idx.contains("- plain: no needs"), "{idx}");
        assert!(!idx.contains("web"), "{idx}");

        let idx = render_index(&found, &tools(&["bash", "browser"]));
        assert!(idx.contains("- web: needs browser"), "{idx}");
    }

    #[test]
    fn index_caps_recipe_count_and_description_length() {
        let recipes: Vec<Recipe> = (0..60)
            .map(|i| Recipe {
                name: format!("r{i:02}"),
                description: "d".repeat(300),
                path: PathBuf::from("RECIPE.md"),
                requires_tools: vec![],
            })
            .collect();
        let idx = render_index(&recipes, &tools(&[]));
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
    fn corrupt_snapshot_is_ignored() {
        let ws = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        write_recipe(ws.path(), "ship", "name: ship\ndescription: deploy", "");
        std::fs::write(cfg.path().join(SNAPSHOT_FILE), "{not json").unwrap();
        let idx = index_in(ws.path(), Some(cfg.path()), &tools(&[]));
        assert!(idx.contains("- ship: deploy"), "{idx}");
    }
}
