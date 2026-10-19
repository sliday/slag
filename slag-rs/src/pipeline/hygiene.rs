use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

pub fn install_ignores(root: &Path, commission: Option<&str>) -> io::Result<()> {
    let path = root.join(".gitignore");
    require_regular_ignore(&path)?;
    let mut body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut stacks = [false; 3];
    let text = commission.unwrap_or_default().to_ascii_lowercase();
    for word in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        match word {
            "python" | "python3" | "pytest" | "unittest" | "pip" | "py" => stacks[0] = true,
            "node" | "nodejs" | "npm" | "javascript" | "typescript" | "react" | "nextjs" | "js"
            | "ts" => stacks[1] = true,
            "rust" | "cargo" | "rs" => stacks[2] = true,
            _ => {}
        }
    }
    detect_stacks(root, 0, &mut 1_000, &mut stacks)?;
    install_local_ignores(root)?;
    let mut patterns = Vec::new();
    if stacks[0] {
        patterns.extend([
            "__pycache__/",
            "*.pyc",
            "*.pyo",
            ".pytest_cache/",
            ".mypy_cache/",
            ".ruff_cache/",
            ".venv/",
            "venv/",
            ".coverage",
            "htmlcov/",
            "build/",
            "dist/",
            "*.egg-info/",
        ]);
    }
    if stacks[1] {
        patterns.extend([
            "node_modules/",
            "dist/",
            "build/",
            ".next/",
            ".nuxt/",
            ".turbo/",
            "coverage/",
        ]);
    }
    if stacks[2] {
        patterns.push("target/");
    }
    let original_len = body.len();
    for pattern in patterns {
        if body
            .lines()
            .any(|line| line.trim() == pattern)
        {
            continue;
        }
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(pattern);
        body.push('\n');
    }
    if body.len() != original_len {
        require_regular_ignore(&path)?;
        std::fs::write(path, body)?;
    }
    Ok(())
}

fn install_local_ignores(root: &Path) -> io::Result<()> {
    let output = crate::engine::sandbox::git()
        .args(["rev-parse", "--git-path", "info/exclude"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("not a git repository") {
            return Ok(());
        }
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr)));
    }
    let relative = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    let path = root.join(relative.trim());
    let root = root.canonicalize()?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("exclude has no parent"))?;
    let mut ancestor = parent;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| io::Error::other("exclude has no existing parent"))?;
    }
    let control = crate::engine::sandbox::git()
        .args(["rev-parse", "--git-common-dir"]).current_dir(&root).output()?;
    if !control.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&control.stderr)));
    }
    let control = String::from_utf8(control.stdout).map_err(io::Error::other)?;
    let control = root.join(control.trim()).canonicalize()?;
    let ancestor = ancestor.canonicalize()?;
    if !ancestor.starts_with(&root) && !ancestor.starts_with(control) {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if ["/.slag/", "/logs/"]
            .iter()
            .all(|pattern| existing.lines().any(|line| line.trim() == *pattern))
        {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "local excludes leave the workspace",
        ));
    }
    for part in parent.strip_prefix(&root).unwrap_or(parent).components() {
        if matches!(part, Component::ParentDir) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "local excludes leave the workspace",
            ));
        }
    }
    require_regular_ignore(&path)?;
    let mut body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let original_len = body.len();
    for pattern in ["/.slag/", "/logs/"] {
        if body.lines().any(|line| line.trim() == pattern) {
            continue;
        }
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(pattern);
        body.push('\n');
    }
    if body.len() != original_len {
        std::fs::create_dir_all(parent)?;
        require_regular_ignore(&path)?;
        std::fs::write(path, body)?;
    }
    Ok(())
}
fn require_regular_ignore(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cannot update a symlink or nonregular ignore file",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn detect_stacks(
    root: &Path,
    depth: usize,
    budget: &mut usize,
    stacks: &mut [bool; 3],
) -> io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_file() {
            match name.as_ref() {
                "pyproject.toml" | "requirements.txt" | "setup.py" | "Pipfile" => stacks[0] = true,
                "package.json" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" => {
                    stacks[1] = true
                }
                "Cargo.toml" | "Cargo.lock" => stacks[2] = true,
                _ => {}
            }
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("py") => stacks[0] = true,
                Some("js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs") => stacks[1] = true,
                Some("rs") => stacks[2] = true,
                _ => {}
            }
        } else if kind.is_dir()
            && depth < 2
            && !matches!(
                name.as_ref(),
                ".git"
                    | "logs"
                    | ".slag"
                    | "__pycache__"
                    | ".venv"
                    | "venv"
                    | "node_modules"
                    | "target"
                    | "build"
                    | "dist"
                    | ".next"
                    | ".nuxt"
                    | ".pytest_cache"
                    | ".mypy_cache"
                    | ".ruff_cache"
            )
        {
            detect_stacks(&path, depth + 1, budget, stacks)?;
        }
    }
    Ok(())
}

pub(super) struct VerificationFiles {
    root: PathBuf,
    files: BTreeSet<PathBuf>,
}

impl VerificationFiles {
    pub(super) fn capture(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let files = visible_files(&root)?;
        Ok(Self { root, files })
    }

    pub(super) fn clean_new(&self, root: &Path) -> io::Result<Vec<PathBuf>> {
        if root.canonicalize()? != self.root {
            return Err(io::Error::other("verification cleanup root changed"));
        }
        let current = visible_files(&self.root)?;
        let created: Vec<_> = current.difference(&self.files).collect();
        for relative in &created {
            require_local_parent(&self.root, relative)?;
        }
        let mut removed = Vec::new();
        for relative in created {
            require_local_parent(&self.root, relative)?;
            let path = self.root.join(relative);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                    std::fs::remove_file(&path)?
                }
                Ok(_) => {
                    return Err(io::Error::other(format!(
                        "verification created an unsupported path: {}",
                        relative.display()
                    )))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
            let mut parent = path.parent();
            while let Some(dir) = parent.filter(|dir| *dir != self.root) {
                match std::fs::remove_dir(dir) {
                    Ok(()) => parent = dir.parent(),
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
                        ) =>
                    {
                        break
                    }
                    Err(e) => return Err(e),
                }
            }
            removed.push(relative.clone());
        }
        Ok(removed)
    }
}

fn require_local_parent(root: &Path, relative: &Path) -> io::Result<()> {
    let mut parent = root.to_path_buf();
    for part in relative.parent().unwrap_or(Path::new("")).components() {
        parent.push(part);
        match std::fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "verification cleanup refuses a symlink or non-directory parent: {}",
                        parent.display()
                    ),
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    if !parent.canonicalize()?.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "verification cleanup parent leaves the workspace",
        ));
    }
    Ok(())
}

fn visible_files(root: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let output = crate::engine::sandbox::git()
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("not a git repository") {
            let mut files = BTreeSet::new();
            snapshot_without_git(root, root, 0, &mut 20_000, &mut files)?;
            return Ok(files);
        }
        return Err(io::Error::other(format!(
            "cannot inspect verification files: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            #[cfg(unix)]
            let name = {
                use std::os::unix::ffi::OsStringExt;
                OsString::from_vec(bytes.to_vec())
            };
            #[cfg(not(unix))]
            let name = OsString::from(std::str::from_utf8(bytes).map_err(io::Error::other)?);
            let path = PathBuf::from(name);
            if !path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err(io::Error::other(
                    "git returned a nonlocal verification path",
                ));
            }
            Ok(path)
        })
        .collect()
}

fn snapshot_without_git(
    root: &Path,
    directory: &Path,
    depth: usize,
    budget: &mut usize,
    files: &mut BTreeSet<PathBuf>,
) -> io::Result<()> {
    if depth > 32 {
        return Err(io::Error::other(
            "verification snapshot exceeds 32 directories",
        ));
    }
    for entry in std::fs::read_dir(directory)? {
        if *budget == 0 {
            return Err(io::Error::other(
                "verification snapshot exceeds 20000 paths",
            ));
        }
        *budget -= 1;
        let entry = entry?;
        if entry.file_name() == ".git" || directory == root && matches!(entry.file_name().to_str(), Some("logs" | ".slag")) {
            continue;
        }
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            snapshot_without_git(root, &path, depth + 1, budget, files)?;
        } else if kind.is_file() || kind.is_symlink() {
            files.insert(
                path.strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let output = crate::engine::sandbox::git()
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn ignore_rules_preserve_existing_text_and_detect_local_stacks() {
        for (file, expected, absent) in [
            ("app.py", "__pycache__/", "node_modules/"),
            ("package.json", "node_modules/", "target/"),
            ("Cargo.toml", "target/", "__pycache__/"),
        ] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join(file), "").unwrap();
            std::fs::write(
                root.path().join(".gitignore"),
                "# keep this\n!important.csv",
            )
            .unwrap();
            install_ignores(root.path(), None).unwrap();
            let first = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
            assert!(
                first.starts_with("# keep this\n!important.csv\n"),
                "{first}"
            );
            assert!(first.lines().any(|line| line == expected), "{first}");
            assert!(!first.lines().any(|line| line == absent), "{first}");
            assert!(!first.contains("*.csv"), "{first}");
            install_ignores(root.path(), None).unwrap();
            assert_eq!(
                std::fs::read_to_string(root.path().join(".gitignore")).unwrap(),
                first
            );
        }
    }

    #[test]
    fn coordinator_ignores_stay_local_and_language_rules_remain_portable() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        install_ignores(root.path(), Some("Python CLI")).unwrap();
        let ignore = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert!(ignore.contains("__pycache__/"));
        assert!(!ignore.contains(".slag"));
        assert!(!ignore.contains("logs"));
        let exclude = root.path().join(".git/info/exclude");
        let first = std::fs::read_to_string(&exclude).unwrap();
        assert!(first.lines().any(|line| line == "/.slag/"));
        assert!(first.lines().any(|line| line == "/logs/"));
        install_ignores(root.path(), Some("Python CLI")).unwrap();
        assert_eq!(std::fs::read_to_string(&exclude).unwrap(), first);
        std::fs::create_dir(root.path().join(".slag")).unwrap();
        std::fs::write(root.path().join(".slag/PLAN.md"), "state").unwrap();
        std::fs::create_dir(root.path().join("logs")).unwrap();
        std::fs::write(root.path().join("logs/trace"), "state").unwrap();
        git(
            root.path(),
            &["check-ignore", ".slag/PLAN.md", "logs/trace"],
        );
    }

    #[test]
    fn local_excludes_support_separate_git_metadata() {
        let root = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        let control = config.path().join("git-metadata");
        std::fs::rename(root.path().join(".git"), &control).unwrap();
        std::fs::write(root.path().join(".git"), format!("gitdir: {}\n", control.display())).unwrap();
        install_ignores(root.path(), None).unwrap();
        let exclude = std::fs::read_to_string(control.join("info/exclude")).unwrap();
        assert!(exclude.lines().any(|line| line == "/.slag/"));
        assert!(exclude.lines().any(|line| line == "/logs/"));
    }

    #[test]
    fn language_neutral_commissions_create_only_local_excludes() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        install_ignores(root.path(), Some("write a greeting")).unwrap();
        assert!(!root.path().join(".gitignore").exists());
        assert!(root.path().join(".git/info/exclude").is_file());
    }

    #[test]
    #[cfg(unix)]
    fn local_excludes_refuse_symlinks_and_external_parents() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        let target = outside.path().join("exclude");
        std::fs::write(&target, "preserve\n").unwrap();
        let exclude = root.path().join(".git/info/exclude");
        std::fs::remove_file(&exclude).unwrap();
        symlink(&target, &exclude).unwrap();
        assert_eq!(
            install_ignores(root.path(), None).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        std::fs::remove_file(&exclude).unwrap();
        std::fs::remove_dir(root.path().join(".git/info")).unwrap();
        symlink(outside.path(), root.path().join(".git/info")).unwrap();
        assert_eq!(
            install_ignores(root.path(), None).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "preserve\n");
    }
    #[test]
    fn commission_detects_the_stack_before_files_exist() {
        for (commission, expected) in [
            ("write wc.py with a unittest", "__pycache__/"),
            ("create a Node utility", "node_modules/"),
            ("write a Rust CLI", "target/"),
        ] {
            let root = tempfile::tempdir().unwrap();
            install_ignores(root.path(), Some(commission)).unwrap();
            let body = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
            assert!(body.lines().any(|line| line == expected), "{body}");
        }
    }

    #[test]
    fn cleanup_removes_only_new_verification_files_including_staged_files() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        std::fs::create_dir(root.path().join("fixtures")).unwrap();
        std::fs::write(root.path().join("fixtures/golden.csv"), "real fixture\n").unwrap();
        std::fs::write(root.path().join("app.py"), "print(1)\n").unwrap();
        git(root.path(), &["add", "app.py"]);
        install_ignores(root.path(), None).unwrap();
        let before = VerificationFiles::capture(root.path()).unwrap();
        std::fs::write(root.path().join("probe.csv"), "generated\n").unwrap();
        std::fs::create_dir(root.path().join("scratch")).unwrap();
        std::fs::write(root.path().join("scratch/generated.csv"), "generated\n").unwrap();
        git(root.path(), &["add", "scratch/generated.csv"]);
        std::fs::create_dir(root.path().join("__pycache__")).unwrap();
        std::fs::write(root.path().join("__pycache__/app.pyc"), "cache").unwrap();
        assert_eq!(before.clean_new(root.path()).unwrap().len(), 2);
        assert_eq!(
            std::fs::read_to_string(root.path().join("fixtures/golden.csv")).unwrap(),
            "real fixture\n"
        );
        assert!(root.path().join("app.py").exists());
        assert!(!root.path().join("probe.csv").exists());
        assert!(!root.path().join("scratch").exists());
        assert!(root.path().join("__pycache__/app.pyc").exists());
    }

    #[test]
    #[cfg(unix)]
    fn ignores_refuse_existing_and_dangling_symlinks_without_changing_the_target() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("ignore");
        std::fs::write(&target, "outside contents\n").unwrap();
        for destination in [&target, &outside.path().join("missing")] {
            let path = root.path().join(".gitignore");
            symlink(destination, &path).unwrap();
            let error = install_ignores(root.path(), Some("Python utility")).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(std::fs::read_link(&path).unwrap(), *destination);
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                "outside contents\n"
            );
            assert!(!outside.path().join("missing").exists());
            std::fs::remove_file(path).unwrap();
        }
        std::fs::create_dir(root.path().join(".gitignore")).unwrap();
        assert_eq!(
            install_ignores(root.path(), None).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    #[cfg(unix)]
    fn cleanup_refuses_staged_paths_below_a_replaced_symlink_parent() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        let before = VerificationFiles::capture(root.path()).unwrap();
        std::fs::create_dir(root.path().join("scratch")).unwrap();
        std::fs::write(root.path().join("scratch/victim"), "generated").unwrap();
        git(root.path(), &["add", "scratch/victim"]);
        std::fs::remove_file(root.path().join("scratch/victim")).unwrap();
        std::fs::remove_dir(root.path().join("scratch")).unwrap();
        std::fs::write(outside.path().join("victim"), "preserve outside\n").unwrap();
        symlink(outside.path(), root.path().join("scratch")).unwrap();
        let error = before.clean_new(root.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::read_to_string(outside.path().join("victim")).unwrap(),
            "preserve outside\n"
        );
        assert!(root.path().join("scratch").is_symlink());
    }

    #[test]
    #[cfg(unix)]
    fn cleanup_unlinks_a_new_leaf_symlink_without_changing_its_target() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        let before = VerificationFiles::capture(root.path()).unwrap();
        let target = outside.path().join("fixture.csv");
        std::fs::write(&target, "preserve outside\n").unwrap();
        symlink(&target, root.path().join("probe.csv")).unwrap();
        assert_eq!(
            before.clean_new(root.path()).unwrap(),
            [PathBuf::from("probe.csv")]
        );
        assert!(!root.path().join("probe.csv").exists());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "preserve outside\n"
        );
    }

    #[test]
    fn cleanup_without_a_repository_preserves_existing_files_and_heap() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("fixture.csv"), "preserve\n").unwrap();
        let before = VerificationFiles::capture(root.path()).unwrap();
        std::fs::write(root.path().join("generated.csv"), "scratch\n").unwrap();
        std::fs::create_dir(root.path().join("logs")).unwrap();
        std::fs::write(root.path().join("logs/trace"), "preserve heap\n").unwrap();
        assert_eq!(
            before.clean_new(root.path()).unwrap(),
            [PathBuf::from("generated.csv")]
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("fixture.csv")).unwrap(),
            "preserve\n"
        );
        assert!(root.path().join("logs/trace").exists());
    }
}
