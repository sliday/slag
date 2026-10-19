use std::io::Write;
use std::path::Path;

pub(crate) fn materialize(command: &str, directory: &Path) -> std::io::Result<String> {
    let trimmed = command.trim();
    let controls = shell_controls(trimmed);
    if let Some((parts, outer)) = controls {
        if parts.len() > 1 {
            return parts.iter().map(|part| materialize(part, directory)).collect::<std::io::Result<Vec<_>>>()
                .map(|parts| parts.join(" && "));
        }
        if outer {
            return materialize(&trimmed[1..trimmed.len() - 1], directory).map(|inner| format!("( {inner} )"));
        }
    }
    if has_shell_syntax(command) {
        return Ok(command.to_string());
    }
    let Ok(mut args) = shell_words::split(command) else {
        return Ok(command.to_string());
    };
    let assignments = args.iter().take_while(|arg| assignment(arg)).count();
    let Some(program) = args.get(assignments) else {
        return Ok(command.to_string());
    };
    let name = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or("");
    if name != "python" && name != "python3" && !name.strip_prefix("python3.").is_some_and(|version| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit())) {
        return Ok(command.to_string());
    }
    let mut index = assignments + 1;
    while let Some(arg) = args.get(index) {
        if arg == "-c" { break; }
        if matches!(arg.as_str(), "-B" | "-E" | "-I" | "-O" | "-OO" | "-P" | "-q" | "-s" | "-S" | "-u" | "-v" | "-b" | "-bb") {
            index += 1;
        } else if matches!(arg.as_str(), "-W" | "-X") && args.get(index + 1).is_some() {
            index += 2;
        } else {
            return Ok(command.to_string());
        }
    }
    let Some(source) = args.get(index + 1) else {
        return Ok(command.to_string());
    };
    let mut file = tempfile::Builder::new().prefix("slag-probe-").suffix(".py").tempfile_in(directory)?;
    // Keep -c imports and argv semantics when the source runs from a file.
    writeln!(file, "import sys as _slag_sys\nif not getattr(_slag_sys.flags, 'safe_path', False): _slag_sys.path[0] = ''\n_slag_sys.argv[0] = '-c'\ndel _slag_sys\ndel __file__")?;
    writeln!(file, "exec(compile({}, '<string>', 'exec'), globals())", serde_json::to_string(source)?)?;
    let (_, path) = file.keep().map_err(|error| error.error)?;
    args.splice(index..=index + 1, [path.to_string_lossy().into_owned()]);
    Ok(args.iter().enumerate().map(|(index, arg)| {
        if index < assignments {
            let (key, value) = arg.split_once('=').unwrap();
            format!("{key}={}", shell_words::quote(value))
        } else {
            shell_words::quote(arg).into_owned()
        }
    }).collect::<Vec<_>>().join(" "))
}

fn shell_controls(command: &str) -> Option<(Vec<&str>, bool)> {
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut outer = command.starts_with('(');
    let mut start = 0;
    let mut parts = Vec::new();
    let mut chars = command.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if escaped { escaped = false; continue; }
        if c == '\\' && quote != Some('\'') { escaped = true; continue; }
        if let Some(delimiter) = quote {
            if c == delimiter { quote = None; }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 && index + 1 != command.len() { outer = false; }
            }
            '&' if depth == 0 && chars.peek().is_some_and(|(_, next)| *next == '&') => {
                chars.next();
                parts.push(command[start..index].trim());
                start = index + 2;
            }
            '#' | '$' | '`' if depth == 0 => return None,
            _ => {},
        }
    }
    if quote.is_some() || escaped || depth != 0 { return None; }
    parts.push(command[start..].trim());
    Some((parts, outer))
}

fn assignment(arg: &str) -> bool {
    arg.split_once('=').is_some_and(|(name, _)| !name.is_empty() && name.chars().enumerate().all(|(index, c)| c == '_' || c.is_ascii_alphabetic() || (index > 0 && c.is_ascii_digit())))
}

fn has_shell_syntax(command: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for c in command.chars() {
        if escaped { escaped = false; continue; }
        if c == '\\' && quote != Some('\'') { escaped = true; continue; }
        if quote == Some('\'') {
            if c == '\'' { quote = None; }
            continue;
        }
        if c == '$' || c == '`' { return true; }
        if quote == Some('"') {
            if c == '"' { quote = None; }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')' | '\n' | '*' | '?' | '[' | '#') {
            return true;
        }
    }
    quote.is_some() || escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_probe_keeps_escaped_quotes_argv_and_workspace_imports() {
        let dir = tempfile::tempdir().unwrap();
        let source = "import sys; import os; print(\"quoted \\\"word\\\"\"); assert sys.argv == ['-c', \"a'b\"]; assert sys.path[0] == ''; assert '__file__' not in globals()";
        let command = format!("PYTHONDONTWRITEBYTECODE=1 python3 -B -c {} {}", shell_words::quote(source), shell_words::quote("a'b"));
        let transformed = materialize(&command, dir.path()).unwrap();
        assert!(!transformed.contains(" -c "));
        let output = std::process::Command::new("bash").args(["-c", &transformed]).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "quoted \"word\"\n");
    }

    #[test]
    fn python_probe_supports_flags_without_safe_path() {
        let dir = tempfile::tempdir().unwrap();
        let transformed = materialize("python3 -c 'assert __import__(\"sys\").path[0] == \"\"'", dir.path()).unwrap();
        let words = shell_words::split(&transformed).unwrap();
        let launcher = "import sys; __file__ = sys.argv[1]; source = open(__file__).read(); sys.flags = object(); exec(source)";
        assert!(std::process::Command::new("python3").args(["-c", launcher, &words[1]]).status().unwrap().success());
    }

    #[test]
    fn python_probe_preserves_future_imports() {
        let dir = tempfile::tempdir().unwrap();
        let transformed = materialize("python3 -c 'from __future__ import annotations; value: Missing = 1'", dir.path()).unwrap();
        assert!(std::process::Command::new("bash").args(["-c", &transformed]).status().unwrap().success());
    }

    #[test]
    fn python_probe_preserves_isolated_mode() {
        let dir = tempfile::tempdir().unwrap();
        let transformed = materialize("python3 -I -c 'import sys; assert sys.flags.isolated; assert sys.argv[0] == \"-c\"'", dir.path()).unwrap();
        assert!(std::process::Command::new("bash").args(["-c", &transformed]).status().unwrap().success());
    }

    #[test]
    fn python_probe_materializes_grouped_proofs() {
        let dir = tempfile::tempdir().unwrap();
        let source = "print(\"parenthesis ) and && stay quoted\")";
        let command = format!("( python3 -c {} ) && ( python3 -c 'assert 2 + 2 == 4' )", shell_words::quote(source));
        let transformed = materialize(&command, dir.path()).unwrap();
        assert!(!transformed.contains(" -c "));
        let output = std::process::Command::new("bash").args(["-c", &transformed]).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn python_probe_does_not_reinterpret_shell_operations() {
        let dir = tempfile::tempdir().unwrap();
        for command in ["python3 -c \"$SOURCE\"", "python3 -m unittest", "python3 -c 'print(1)' > result"] {
            assert_eq!(materialize(command, dir.path()).unwrap(), command);
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
