//! The issue a fix works from: free text, a file, or a GitHub issue.

use std::path::Path;

use crate::error::SlagError;

/// Comments past this many characters each are cut. A long thread pays
/// for every turn of the session, and the first few comments carry the
/// reproduction steps when there are any.
const COMMENT_CAP: usize = 1_500;
/// How many comments from a GitHub thread reach the prompt.
const MAX_COMMENTS: usize = 5;

/// What to say when no issue was given.
pub const USAGE: &str = "no issue given: `slag fix \"what is broken\"`, `slag fix 123`, \
    `slag fix https://github.com/owner/repo/issues/123`, or `slag fix --issue-file ISSUE.md`";

/// One issue, however it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub title: String,
    pub body: String,
    /// GitHub issue number, when the issue came from GitHub.
    pub number: Option<u64>,
    pub url: Option<String>,
    /// `owner/repo` when the issue came by URL and may live elsewhere.
    pub repo: Option<String>,
}

impl Issue {
    /// Free text: the first non-empty line is the title, the whole text the
    /// body, so a one-line issue reads the same in both places.
    pub fn from_text(text: &str) -> Self {
        let text = text.trim();
        let title = text
            .lines()
            .map(|l| l.trim().trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        Self {
            title: crate::tui::truncate(title, 100),
            body: text.to_string(),
            number: None,
            url: None,
            repo: None,
        }
    }

    /// How a commit or PR names this issue so that merging closes it:
    /// `#12` here, `owner/repo#12` when the URL named a repository.
    pub fn closing_ref(&self) -> Option<String> {
        let n = self.number?;
        Some(match &self.repo {
            Some(repo) => format!("{repo}#{n}"),
            None => format!("#{n}"),
        })
    }

    /// The issue as the smith reads it.
    pub fn render(&self) -> String {
        let mut s = String::new();
        if let Some(n) = self.number {
            s.push_str(&format!("#{n} {}\n", self.title));
        }
        if let Some(url) = &self.url {
            s.push_str(&format!("{url}\n"));
        }
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(self.body.trim());
        s
    }

    /// `slag/fix-<n>-<slug>` for a GitHub issue, `slag/fix-<slug>` otherwise.
    pub fn branch_name(&self) -> String {
        match self.number {
            Some(n) => format!("slag/fix-{n}-{}", slug(&self.title)),
            None => format!("slag/fix-{}", slug(&self.title)),
        }
    }
}

/// What the ISSUE argument names.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    Text(String),
    /// An issue number in the current repository.
    Number(u64),
    /// An issue URL, which may point at another repository.
    Url { repo: String, number: u64 },
}

/// Read the ISSUE words. A lone number (`123`, `#123`) or a GitHub issue
/// URL is fetched; anything else is the issue itself.
pub fn classify(words: &[String]) -> Source {
    let text = words.join(" ");
    let t = text.trim();
    if !t.contains(char::is_whitespace) {
        if let Ok(n) = t.strip_prefix('#').unwrap_or(t).parse::<u64>() {
            return Source::Number(n);
        }
        if let Some(source) = parse_issue_url(t) {
            return source;
        }
    }
    Source::Text(t.to_string())
}

/// `https://github.com/<owner>/<repo>/issues/<n>`, with or without a
/// trailing slash, query, or comment anchor.
fn parse_issue_url(url: &str) -> Option<Source> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("github.com/"))?;
    let rest = rest.split(['?', '#']).next()?.trim_end_matches('/');
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [owner, repo, "issues", n] if !owner.is_empty() && !repo.is_empty() => Some(Source::Url {
            repo: format!("{owner}/{repo}"),
            number: n.parse().ok()?,
        }),
        _ => None,
    }
}

/// Resolve the ISSUE argument and `--issue-file` into one issue.
pub async fn resolve(words: &[String], file: Option<&Path>) -> Result<Issue, SlagError> {
    let given = words.iter().any(|w| !w.trim().is_empty());
    if let Some(path) = file {
        if given {
            return Err(SlagError::Config(
                "pass the issue as text or with --issue-file, not both".into(),
            ));
        }
        let text = std::fs::read_to_string(path).map_err(|e| {
            SlagError::Config(format!("cannot read the issue file {}: {e}", path.display()))
        })?;
        if text.trim().is_empty() {
            return Err(SlagError::Config(format!("the issue file {} is empty", path.display())));
        }
        return Ok(Issue::from_text(&text));
    }
    match classify(words) {
        Source::Text(t) if t.is_empty() => Err(SlagError::Config(USAGE.into())),
        Source::Text(t) => Ok(Issue::from_text(&t)),
        Source::Number(n) => fetch(n, None).await,
        Source::Url { repo, number } => fetch(number, Some(&repo)).await,
    }
}

/// Fetch a GitHub issue with `gh issue view`.
async fn fetch(number: u64, repo: Option<&str>) -> Result<Issue, SlagError> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.envs(crate::engine::sandbox::GIT_GUARD_ENV);
    cmd.args(["issue", "view", &number.to_string(), "--json", "number,title,body,url,comments"]);
    if let Some(repo) = repo {
        cmd.args(["--repo", repo]);
    }
    let out = cmd.output().await.map_err(|e| {
        SlagError::Config(format!("issue #{number} needs the GitHub CLI (`gh`): {e}"))
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(SlagError::Config(format!(
            "gh could not fetch issue #{number}: {}",
            err.trim()
        )));
    }
    let mut issue = parse_gh_json(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| {
        SlagError::Config(format!("gh returned unreadable JSON for issue #{number}"))
    })?;
    issue.repo = repo.map(str::to_string);
    Ok(issue)
}

/// Parse `gh issue view --json number,title,body,url,comments`.
fn parse_gh_json(raw: &str) -> Option<Issue> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let title = v.get("title")?.as_str()?.trim().to_string();
    let mut body = v.get("body").and_then(|b| b.as_str()).unwrap_or_default().trim().to_string();
    if body.is_empty() {
        body = title.clone();
    }
    let comments: Vec<String> = v
        .get("comments")
        .and_then(|c| c.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let text = c.get("body")?.as_str()?.trim();
                    let who = c
                        .pointer("/author/login")
                        .and_then(|a| a.as_str())
                        .unwrap_or("someone");
                    (!text.is_empty())
                        .then(|| format!("{who}: {}", crate::tui::truncate(text, COMMENT_CAP)))
                })
                .take(MAX_COMMENTS)
                .collect()
        })
        .unwrap_or_default();
    if !comments.is_empty() {
        body.push_str("\n\nComments:\n");
        body.push_str(&comments.join("\n\n"));
    }
    Some(Issue {
        title: crate::tui::truncate(&title, 100),
        body: format!("{title}\n\n{body}"),
        number: v.get("number").and_then(|n| n.as_u64()),
        url: v.get("url").and_then(|u| u.as_str()).map(str::to_string),
        repo: None,
    })
}

/// Words a branch name should not end on.
const STOP_WORDS: &[&str] = &["a", "an", "and", "at", "for", "in", "of", "on", "or", "the", "to", "with"];

/// A branch-safe slug: lowercase ASCII words joined by `-`, at most six
/// words and 40 characters. A title with no usable word becomes `issue`.
pub fn slug(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .take(6)
        .collect();
    // A slug that stops mid-phrase reads like a typo: drop the filler a
    // six-word cut leaves dangling at the end.
    let keep = words
        .iter()
        .rposition(|w| !STOP_WORDS.contains(&w.as_str()))
        .map_or(0, |last| last + 1);
    let mut slug = String::new();
    for word in &words[..keep] {
        if !slug.is_empty() && slug.len() + 1 + word.len() > 40 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(word);
    }
    if slug.is_empty() {
        slug.push_str("issue");
    }
    slug.truncate(40);
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn numbers_and_urls_are_fetched_text_is_the_issue() {
        assert_eq!(classify(&words("123")), Source::Number(123));
        assert_eq!(classify(&words("#42")), Source::Number(42));
        assert_eq!(
            classify(&words("https://github.com/acme/app/issues/7#issuecomment-1")),
            Source::Url { repo: "acme/app".into(), number: 7 }
        );
        assert_eq!(
            classify(&words("dates off by one in export")),
            Source::Text("dates off by one in export".into())
        );
        // A pull request URL is not an issue, and a number inside prose
        // is part of the prose.
        assert!(matches!(
            classify(&words("https://github.com/acme/app/pull/7")),
            Source::Text(_)
        ));
        assert!(matches!(classify(&words("fix 123 now")), Source::Text(_)));
    }

    #[test]
    fn free_text_titles_come_from_the_first_line() {
        let issue = Issue::from_text("\n# Export drops the last day\n\nDetails here.\n");
        assert_eq!(issue.title, "Export drops the last day");
        assert!(issue.body.contains("Details here."));
        assert_eq!(issue.branch_name(), "slag/fix-export-drops-the-last-day");
    }

    #[test]
    fn slugs_are_short_and_branch_safe() {
        assert_eq!(slug("Dates off by one in CSV export (UTC)"), "dates-off-by-one-in-csv");
        assert_eq!(slug("¿¡!!"), "issue");
        assert_eq!(slug("Export drops the last day of the range"), "export-drops-the-last-day");
        assert!(slug(&"word ".repeat(40)).len() <= 40);
    }

    #[test]
    fn github_issues_carry_their_number_into_the_branch() {
        let raw = r#"{"number": 12, "title": "Crash on empty input", "body": "Steps: run it.",
            "url": "https://github.com/acme/app/issues/12",
            "comments": [{"author": {"login": "ana"}, "body": "Also on 2.1"}]}"#;
        let issue = parse_gh_json(raw).unwrap();
        assert_eq!(issue.number, Some(12));
        assert_eq!(issue.branch_name(), "slag/fix-12-crash-on-empty-input");
        assert!(issue.body.contains("ana: Also on 2.1"), "{}", issue.body);
        assert!(issue.render().starts_with("#12 Crash on empty input"));
        assert_eq!(issue.closing_ref().as_deref(), Some("#12"));
        let elsewhere = Issue { repo: Some("acme/app".into()), ..issue };
        assert_eq!(elsewhere.closing_ref().as_deref(), Some("acme/app#12"));
    }

    #[tokio::test]
    async fn text_and_a_file_together_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ISSUE.md");
        std::fs::write(&path, "broken").unwrap();
        assert!(resolve(&words("also this"), Some(&path)).await.is_err());
        assert_eq!(resolve(&[], Some(&path)).await.unwrap().title, "broken");
        assert!(resolve(&[], None).await.is_err(), "no issue at all is refused");
    }
}
