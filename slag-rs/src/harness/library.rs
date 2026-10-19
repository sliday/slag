//! The harness library (Harness² §2.3 and appendix D.2): the harness each
//! evolved task adopted, with the experience behind it, per domain, with
//! an index.
//!
//! ```text
//! <library>/<domain>/INDEX.md                      one line per entry
//! <library>/<domain>/<entry>/EXPERIENCE.md         cue, lesson, evidence, edit rows
//! <library>/<domain>/<entry>/harness/              the adopted tree, executable as is
//! <library>/<domain>/<entry>/candidates/<label>/   a candidate tree whose edit backs the lesson
//! ```
//!
//! The library lives per user (`<config dir>/library`), or wherever
//! `--library`, `SLAG_HARNESS_LIBRARY` or `harness_library` points, and a
//! repository may keep its own at `.slag/library`. A domain is the kind of
//! repository (`python`, `node`, `rust`, `go`, `shell`, `general`), so a
//! lesson from one Python repository reaches the next.

use std::path::{Path, PathBuf};

use super::{tree, write_tree};

/// How many entries an editor's evidence carries: the newest ones.
const SNAPSHOT_ENTRIES: usize = 8;
/// How many entries the cheap path draws recipes from.
const APPLY_ENTRIES: usize = 3;

/// One harness library on disk.
#[derive(Debug, Clone)]
pub struct Library {
    pub root: PathBuf,
}

/// One stored entry, as its EXPERIENCE.md reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub dir: PathBuf,
    pub title: String,
    pub repository: String,
    pub cue: String,
    pub lesson: String,
    pub confidence: String,
}

/// The reflector's one transferable lesson.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lesson {
    pub cue: String,
    pub lesson: String,
    pub evidence: String,
    pub confidence: String,
}

/// One row of the reflector's edit attribution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditRow {
    pub component: String,
    pub candidates: String,
    pub change: String,
    pub observed: String,
    pub verdict: String,
}

/// Everything a new entry records.
pub struct NewEntry<'a> {
    pub title: &'a str,
    pub repository: &'a str,
    pub adopted: &'a str,
    pub harness: &'a Path,
    pub lesson: &'a Lesson,
    pub rows: &'a [EditRow],
    /// Candidate trees, by label, that carried an edit the lesson rests on
    /// and were not adopted. An edit from a rejected candidate can be the
    /// one that helped, and the library keeps it executable.
    pub evidence: &'a [(String, PathBuf)],
}

impl Library {
    /// The library to use: `flag`, then `SLAG_HARNESS_LIBRARY` or
    /// `harness_library` in config, then the repository's own
    /// `.slag/library` when it exists, then `<config dir>/library`.
    pub fn resolve(flag: Option<&Path>, repo: &Path) -> Option<Self> {
        if let Some(dir) = flag {
            return Some(Self { root: dir.to_path_buf() });
        }
        if let Some(dir) = crate::config::harness_library() {
            return Some(Self { root: dir });
        }
        let own = repo.join(".slag").join("library");
        if own.is_dir() {
            return Some(Self { root: own });
        }
        crate::config::config_dir_path().map(|d| Self { root: d.join("library") })
    }

    pub fn domain_dir(&self, domain: &str) -> PathBuf {
        self.root.join(clean(domain))
    }

    /// The domain's entries, newest first.
    pub fn entries(&self, domain: &str) -> Vec<Entry> {
        let Ok(dirs) = std::fs::read_dir(self.domain_dir(domain)) else { return Vec::new() };
        let mut out: Vec<Entry> = dirs
            .flatten()
            .filter_map(|d| {
                let text = std::fs::read_to_string(d.path().join("EXPERIENCE.md")).ok()?;
                Some(parse_entry(&d.file_name().to_string_lossy(), &d.path(), &text))
            })
            .collect();
        out.sort_by(|a, b| b.id.cmp(&a.id));
        out
    }

    /// Copy the domain's index and newest entries into `dst`, for an
    /// improver session to read. Returns how many entries went in.
    pub fn snapshot(&self, domain: &str, dst: &Path) -> usize {
        let entries = self.entries(domain);
        let _ = std::fs::create_dir_all(dst);
        let index = std::fs::read_to_string(self.domain_dir(domain).join("INDEX.md"))
            .unwrap_or_else(|_| format!("# Harness library: {domain}\n\nNo entries yet.\n"));
        let _ = std::fs::write(dst.join("INDEX.md"), index);
        let mut n = 0;
        for e in entries.iter().take(SNAPSHOT_ENTRIES) {
            let mut files = tree(&e.dir);
            files.retain(|p, _| p == "EXPERIENCE.md" || p.starts_with("harness/") || p.starts_with("candidates/"));
            if write_tree(&files, &dst.join(&e.id)).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Store an entry and rewrite the domain's index. Returns its dir.
    pub fn add(&self, domain: &str, entry: &NewEntry) -> std::io::Result<PathBuf> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let base = format!("{stamp}-{}", crate::pipeline::fix::slug(entry.title));
        let mut id = base.clone();
        let mut n = 2;
        while self.domain_dir(domain).join(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        let dir = self.domain_dir(domain).join(&id);
        write_tree(&tree(entry.harness), &dir.join("harness"))?;
        for (label, tree_dir) in entry.evidence {
            write_tree(&tree(tree_dir), &dir.join("candidates").join(clean(label)))?;
        }
        std::fs::write(dir.join("EXPERIENCE.md"), experience_md(domain, entry))?;
        self.write_index(domain)?;
        Ok(dir)
    }

    fn write_index(&self, domain: &str) -> std::io::Result<()> {
        let mut s = format!(
            "# Harness library: {domain}\n\nOne entry per evolved task. Each holds the harness \
             it adopted (`harness/`) and EXPERIENCE.md, which says when the lesson applies and \
             what evidence backs it. Newest first.\n\n"
        );
        for e in self.entries(domain) {
            let parts = super::Harness::load(&e.dir.join("harness")).summary();
            s.push_str(&format!(
                "- {}: cue: {} | lesson: {} | confidence: {} | harness: {}\n",
                e.id,
                one_line(&e.cue),
                one_line(&e.lesson),
                one_line(&e.confidence),
                parts
            ));
        }
        std::fs::write(self.domain_dir(domain).join("INDEX.md"), s)
    }

    /// The cheap path (`--harness library`): build a harness in `dst` from
    /// the domain's most relevant entries, with no model call. Entries
    /// rank by the words their title, cue and lesson share with `query`,
    /// with a bonus for the same repository; ties keep the newer entry.
    ///
    /// Only what transfers without a probe comes over. An adopted tree's
    /// always-on procedure, guardrails, tool guidance and hooks were
    /// written for the task that adopted them, and only `--evolve` adapts
    /// them. Recipes, with their own scripts, cost nothing until their
    /// trigger loads one, so they come from every relevant entry,
    /// candidate trees included. From the same repository, the `memory.md`
    /// lines that state a fact "in this repository" come too. Returns the
    /// ids that gave anything, best first.
    pub fn apply(&self, domain: &str, query: &str, repository: &str, dst: &Path) -> Vec<String> {
        let mut scored: Vec<(usize, Entry)> = self
            .entries(domain)
            .into_iter()
            .map(|e| {
                let text = format!("{} {} {}", e.title, e.cue, e.lesson);
                let mut score = overlap(query, &text);
                if e.repository == repository {
                    score += 3;
                }
                (score, e)
            })
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        let mut used = Vec::new();
        let mut files: std::collections::BTreeMap<String, Vec<u8>> = std::collections::BTreeMap::new();
        let mut facts: Vec<String> = Vec::new();
        let mut claimed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (i, (score, e)) in scored.into_iter().take(APPLY_ENTRIES).enumerate() {
            if i > 0 && score == 0 {
                break;
            }
            let mut gave = false;
            // The adopted tree first, then the candidate trees behind the lesson.
            let mut trees = vec![e.dir.join("harness")];
            if let Ok(dirs) = std::fs::read_dir(e.dir.join("candidates")) {
                let mut more: Vec<PathBuf> = dirs.flatten().map(|d| d.path()).collect();
                more.sort();
                trees.extend(more);
            }
            // A recipe name belongs to the first tree that brings it.
            for t in trees {
                let mut mine = std::collections::BTreeSet::new();
                for (path, bytes) in tree(&t) {
                    let Some(name) = recipe_name(&path) else { continue };
                    if claimed.contains(&name) {
                        continue;
                    }
                    mine.insert(name);
                    files.insert(path, bytes);
                    gave = true;
                }
                claimed.extend(mine);
            }
            if e.repository == repository {
                let memory = std::fs::read_to_string(e.dir.join("harness").join("memory.md")).unwrap_or_default();
                for line in memory.lines().filter(|l| l.to_ascii_lowercase().contains("in this repository")) {
                    if !facts.iter().any(|f| f == line) {
                        facts.push(line.to_string());
                        gave = true;
                    }
                }
            }
            if gave {
                used.push(e.id);
            }
        }
        if !facts.is_empty() {
            files.insert("memory.md".into(), format!("{}\n", facts.join("\n")).into_bytes());
        }
        let _ = write_tree(&files, dst);
        used
    }
}

/// The library's domain for a repository: the stack its tests run on.
pub fn domain_of(root: &Path, test: Option<&str>) -> String {
    let cmd = test.unwrap_or_default().to_ascii_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| cmd.contains(w));
    let has = |f: &str| root.join(f).exists();
    let domain = if says(&["cargo"]) || (cmd.is_empty() && has("Cargo.toml")) {
        "rust"
    } else if says(&["go test"]) || (cmd.is_empty() && has("go.mod")) {
        "go"
    } else if says(&["npm", "node", "yarn", "pnpm", "jest", "vitest", "mocha", "bun "]) || (cmd.is_empty() && has("package.json")) {
        "node"
    } else if says(&["python", "pytest", "unittest", "tox"])
        || (cmd.is_empty() && (has("pyproject.toml") || has("setup.py") || has("requirements.txt")))
    {
        "python"
    } else if says(&["bats", ".sh", "bash ", "sh "]) {
        "shell"
    } else if has("Cargo.toml") {
        "rust"
    } else if has("package.json") {
        "node"
    } else if has("pyproject.toml") || has("setup.py") {
        "python"
    } else {
        "general"
    };
    domain.to_string()
}

/// The reflector's reply: the edit rows, and the lesson unless it wrote
/// NO_ENTRY.
pub fn parse_reflection(text: &str) -> (Vec<EditRow>, Option<Lesson>) {
    let block = |tag: &str| {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = text.rfind(&open)? + open.len();
        let end = text[start..].find(&close).map_or(text.len(), |e| start + e);
        Some(text[start..end].to_string())
    };
    let mut rows = Vec::new();
    if let Some(edits) = block("edits") {
        let mut row = EditRow::default();
        for line in edits.lines() {
            let Some((key, value)) = line.split_once(':') else { continue };
            let value = value.trim().to_string();
            match key.trim().to_ascii_lowercase().as_str() {
                "component" => {
                    if !row.component.is_empty() {
                        rows.push(std::mem::take(&mut row));
                    }
                    row.component = value;
                }
                "candidates" => row.candidates = value,
                "change" => row.change = value,
                "observed" => row.observed = value,
                "verdict" => row.verdict = value.to_ascii_lowercase(),
                _ => {}
            }
        }
        if !row.component.is_empty() {
            rows.push(row);
        }
    }
    let lesson = block("experience").and_then(|exp| {
        let field = |name: &str| {
            exp.lines()
                .find_map(|l| l.trim().strip_prefix(&format!("{name}:")).map(|v| v.trim().to_string()))
                .unwrap_or_default()
        };
        let lesson = Lesson {
            cue: field("cue"),
            lesson: field("lesson"),
            evidence: field("evidence"),
            confidence: field("confidence"),
        };
        (!lesson.cue.is_empty() && !lesson.lesson.is_empty()).then_some(lesson)
    });
    let declined = text.lines().any(|l| l.trim() == "NO_ENTRY");
    (rows, if declined && block("experience").is_none() { None } else { lesson })
}

/// Does the reflection earn a library entry? Only a lesson backed by an
/// observed contrast does: some candidate changed the harness, and at
/// least one `helped` or `hurt` row is one slag's own measurements back
/// (`backed`, a subset of `rows`). `Err` says why not.
pub fn admit(lesson: Option<&Lesson>, edited: bool, rows: &[EditRow], backed: &[EditRow]) -> Result<(), String> {
    if lesson.is_none() {
        return Err("the reflector wrote NO_ENTRY".into());
    }
    if !edited {
        return Err("no candidate changed the harness".into());
    }
    if !rows.iter().any(|r| r.verdict.starts_with("helped") || r.verdict.starts_with("hurt")) {
        return Err("no edit row shows an effect (all confounded or none observed)".into());
    }
    if backed.is_empty() {
        return Err("no `helped` or `hurt` row matches slag's measurements: the runs that carried \
                    the edit did not beat, or fall behind, the reference runs"
            .into());
    }
    let lesson = lesson.map(|l| l.lesson.to_ascii_lowercase()).unwrap_or_default();
    let names_one = COMPONENT_WORDS.iter().any(|w| lesson.contains(w))
        || backed.iter().any(|r| lesson.contains(&r.component.to_ascii_lowercase()));
    if !names_one {
        return Err("the lesson names no harness component, so it says how to fix one issue, \
                    not how to change a harness"
            .into());
    }
    Ok(())
}

/// Words that name a harness component in a lesson.
const COMPONENT_WORDS: [&str; 9] = [
    "systemprompt",
    "system prompt",
    "guardrail",
    "memory",
    "tool_guidance",
    "tool guidance",
    "recipe",
    "script",
    "hook",
];

fn experience_md(domain: &str, e: &NewEntry) -> String {
    let mut s = format!(
        "# {}\n\n- domain: {domain}\n- repository: {}\n- adopted: {}\n- recorded: {}\n\n\
         ## Lesson\n\ncue: {}\nlesson: {}\nevidence: {}\nconfidence: {}\n\n## What each edit did\n",
        one_line(e.title),
        e.repository,
        e.adopted,
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        one_line(&e.lesson.cue),
        one_line(&e.lesson.lesson),
        one_line(&e.lesson.evidence),
        one_line(&e.lesson.confidence),
    );
    for r in e.rows {
        s.push_str(&format!(
            "\ncomponent: {}\ncandidates: {}\nchange: {}\nobserved: {}\nverdict: {}\n",
            r.component, r.candidates, r.change, r.observed, r.verdict
        ));
    }
    s.push_str(&format!(
        "\n## Harness\n\nThe adopted harness is in `harness/`: {}.\n",
        super::Harness::load(e.harness).summary()
    ));
    for (label, dir) in e.evidence {
        s.push_str(&format!(
            "The lesson also rests on `{label}`, which was not adopted; its tree is in \
             `candidates/{}/`: {}.\n",
            clean(label),
            super::Harness::load(dir).summary()
        ));
    }
    s
}

fn parse_entry(id: &str, dir: &Path, text: &str) -> Entry {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| {
                let l = l.trim().trim_start_matches("- ");
                l.strip_prefix(&format!("{name}:")).map(|v| v.trim().to_string())
            })
            .unwrap_or_default()
    };
    Entry {
        id: id.to_string(),
        dir: dir.to_path_buf(),
        title: text.lines().next().unwrap_or_default().trim_start_matches("# ").to_string(),
        repository: field("repository"),
        cue: field("cue"),
        lesson: field("lesson"),
        confidence: field("confidence"),
    }
}

/// `recipes/<name>/...` names its recipe.
fn recipe_name(path: &str) -> Option<String> {
    path.strip_prefix("recipes/")?.split('/').next().map(str::to_string)
}

/// How many distinct words of four letters or more two texts share.
fn overlap(a: &str, b: &str) -> usize {
    let words = |t: &str| -> std::collections::BTreeSet<String> {
        t.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 4)
            .map(str::to_ascii_lowercase)
            .collect()
    };
    words(a).intersection(&words(b)).count()
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A domain name safe as one path segment.
fn clean(domain: &str) -> String {
    let c: String = domain
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if c.is_empty() {
        "general".into()
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFLECTION: &str = "Reading the runs.\n<edits>\ncomponent: recipes/dates/RECIPE.md\ncandidates: dom-1, comp-1\n\
        change: a recipe that tests both range ends\nobserved: dom-1 added a test for the last day; base-1 did not\n\
        verdict: helped\ncomponent: systemprompt.md\ncandidates: inv-1\nchange: reproduce first\nobserved: one run only\n\
        verdict: confounded\n</edits>\n<experience>\ncue: an issue reports a range that loses an end\n\
        lesson: add a recipe that makes the agent assert both range ends (recipes/)\n\
        evidence: row 1; dom-1's new test failed without the fix, base-1's passed\nconfidence: medium, one run\n</experience>\n";

    #[test]
    fn a_reflection_parses_into_rows_and_a_lesson() {
        let (rows, lesson) = parse_reflection(REFLECTION);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].component, "recipes/dates/RECIPE.md");
        assert_eq!(rows[0].verdict, "helped");
        assert_eq!(rows[1].verdict, "confounded");
        let lesson = lesson.expect("a lesson");
        assert!(lesson.cue.starts_with("an issue reports a range"));
        assert!(lesson.confidence.starts_with("medium"));
        let (_, none) = parse_reflection("<edits>\ncomponent: a\nverdict: hurt\n</edits>\nNO_ENTRY\n");
        assert!(none.is_none());
    }

    #[test]
    fn only_a_contrast_backed_lesson_is_admitted() {
        let (rows, lesson) = parse_reflection(REFLECTION);
        let backed = &rows[..1];
        assert!(admit(lesson.as_ref(), true, &rows, backed).is_ok());
        assert!(admit(None, true, &rows, backed).unwrap_err().contains("NO_ENTRY"));
        assert!(admit(lesson.as_ref(), false, &rows, backed).unwrap_err().contains("changed the harness"));
        assert!(admit(lesson.as_ref(), true, &rows, &[]).unwrap_err().contains("slag's measurements"));
        let confounded: Vec<EditRow> = rows.iter().filter(|r| r.verdict == "confounded").cloned().collect();
        assert!(admit(lesson.as_ref(), true, &confounded, &[]).unwrap_err().contains("confounded"));
        let fix_only = Lesson { lesson: "edit split() to hand out the remainder".into(), ..lesson.clone().unwrap() };
        assert!(admit(Some(&fix_only), true, &rows, backed).unwrap_err().contains("names no harness component"));
    }

    #[test]
    fn an_entry_is_stored_indexed_and_applied_by_relevance() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library { root: dir.path().join("lib") };
        let harness = dir.path().join("h");
        let mut files = std::collections::BTreeMap::new();
        files.insert("memory.md".to_string(), b"Tests use unittest.\nIn this repository, dates live in src/d.py.\n".to_vec());
        files.insert(
            "recipes/ends/RECIPE.md".to_string(),
            b"---\nname: ends\ndescription: load this when a range loses an end\n---\nAssert both ends.\n".to_vec(),
        );
        write_tree(&files, &harness).unwrap();
        let (rows, lesson) = parse_reflection(REFLECTION);
        let rejected = dir.path().join("dom");
        let mut other = std::collections::BTreeMap::new();
        other.insert(
            "recipes/dates/RECIPE.md".to_string(),
            b"---\nname: dates\ndescription: load this when dates cross a month\n---\nCheck both months.\n".to_vec(),
        );
        write_tree(&other, &rejected).unwrap();
        let evidence = vec![("dom-1".to_string(), rejected.clone())];
        let entry = NewEntry {
            title: "export drops the last day",
            repository: "shop",
            adopted: "comp-1",
            harness: &harness,
            lesson: lesson.as_ref().unwrap(),
            rows: &rows,
            evidence: &evidence,
        };
        let stored = lib.add("python", &entry).unwrap();
        assert!(stored.join("harness/recipes/ends/RECIPE.md").exists());
        assert!(stored.join("candidates/dom-1/recipes/dates/RECIPE.md").exists(), "a rejected candidate's backing edit is kept");
        let exp = std::fs::read_to_string(stored.join("EXPERIENCE.md")).unwrap();
        assert!(exp.contains("cue: an issue reports a range") && exp.contains("verdict: helped"), "{exp}");
        assert!(exp.contains("candidates/dom-1/"), "{exp}");
        let index = std::fs::read_to_string(lib.domain_dir("python").join("INDEX.md")).unwrap();
        assert!(index.contains("export-drops-the-last-day") && index.contains("recipe ends"), "{index}");
        let listed = lib.entries("python");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].repository, "shop");

        // Another repository gets the recipes only, the rejected
        // candidate's included: always-on text was written for its task.
        let out = dir.path().join("applied");
        let used = lib.apply("python", "the range report loses its last day", "other", &out);
        assert_eq!(used.len(), 1);
        assert!(out.join("recipes/ends/RECIPE.md").exists() && out.join("recipes/dates/RECIPE.md").exists());
        assert!(!out.join("memory.md").exists());
        // The same repository also gets its facts, and only those lines.
        lib.apply("python", "anything", "shop", &out);
        let memory = std::fs::read_to_string(out.join("memory.md")).unwrap();
        assert!(memory.contains("src/d.py") && !memory.contains("unittest"), "{memory}");
        // An empty domain applies nothing.
        assert!(lib.apply("rust", "x", "shop", &dir.path().join("none")).is_empty());

        let snap = dir.path().join("snap");
        assert_eq!(lib.snapshot("python", &snap), 1);
        assert!(snap.join("INDEX.md").exists());
    }

    #[test]
    fn the_domain_follows_the_test_command_then_the_manifests() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(domain_of(dir.path(), Some("python3 -m unittest")), "python");
        assert_eq!(domain_of(dir.path(), Some("node --test")), "node");
        assert_eq!(domain_of(dir.path(), Some("cargo test")), "rust");
        assert_eq!(domain_of(dir.path(), Some("bash tests/run.sh")), "shell");
        assert_eq!(domain_of(dir.path(), None), "general");
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(domain_of(dir.path(), None), "node");
    }
}
