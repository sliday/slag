use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    actual: Vec<String>,
    command: Vec<String>,
    comparison: Comparison,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Comparison { Bytes, Words }

#[derive(Default)]
pub(super) struct Checks {
    pub commands: Vec<String>,
    pub fixtures: Vec<(String, String)>,
    pub criterion: Option<String>,
}

pub(super) fn named_tool(commission: &str) -> Option<String> {
    let pattern = regex::Regex::new(r#"(?i)\b(?:like|compatible with|equivalent to)\s+(?:(?:the|unix|gnu|bsd|posix)\s+)*[`'"]?([a-z][a-z0-9_-]*)"#).ok()?;
    Some(pattern.captures(commission)?[1].to_string())
}

pub(super) fn checks(commission: &str, reference: Option<Reference>) -> Result<Checks, &'static str> {
    let Some(tool) = named_tool(commission) else {
        return if reference.is_none() { Ok(Checks::default()) } else { Err("the commission names no reference tool") };
    };
    let reference = reference.ok_or("the named reference mapping is missing")?;
    if reference.command.first() != Some(&tool) { return Err("reference.command must start with the named reference tool") }
    if reference.actual.first() == Some(&tool) { return Err("reference.actual must launch the product, not the reference tool") }
    for argv in [&reference.actual, &reference.command] {
        if argv.is_empty() || argv.len() > 16 || argv.iter().filter(|arg| arg.as_str() == "{input}").count() != 1
            || argv.iter().any(|arg| arg.is_empty() || arg.len() > 256 || arg.chars().any(char::is_control))
            || crate::engine::agent::review_command_error(&shell_words::join(argv)).is_some()
        { return Err("reference argv must contain one {input}, safe nonempty arguments, and no write command"); }
    }
    if reference.actual.iter().any(|arg| coordinator_probe(arg)) {
        return Err("reference.actual must launch the product, not a coordinator probe");
    }
    if reference.actual.first().is_some_and(|arg| arg == "{input}") {
        return Err("reference.actual must start with the product executable, not {input}");
    }
    let mut checks = Checks::default();
    for (case, text) in [("empty", ""), ("no-final-newline", "alpha beta\nalpha")] {
        let path = format!(".slag/checks/reference-{case}.txt");
        let replace = |argv: &[String]| argv.iter().map(|arg| {
            if arg == "{input}" { path.clone() } else { arg.clone() }
        }).collect::<Vec<_>>();
        let config = serde_json::json!({
            "a": replace(&reference.actual), "r": replace(&reference.command),
            "p": path, "t": text, "w": matches!(reference.comparison, Comparison::Words),
        }).to_string();
        let code = r#"import json,pathlib,subprocess,sys; c=json.loads(sys.argv[1]); assert pathlib.Path(c["p"]).read_bytes()==c["t"].encode(), "reference fixture changed"; a=subprocess.run(c["a"],capture_output=True,timeout=5); r=subprocess.run(c["r"],capture_output=True,timeout=5); n=lambda b:b.split() if c["w"] else b; assert (a.returncode,n(a.stdout))==(r.returncode,n(r.stdout)), (a.returncode,a.stdout,a.stderr,r.returncode,r.stdout,r.stderr); print("reference boundary matched")"#;
        checks.commands.push(shell_words::join(["python3", "-c", code, &config]));
        checks.fixtures.push((path, text.to_string()));
    }
    checks.criterion = Some(format!(
        "Match {tool}'s documented behavior on empty input and a final unterminated text record. Run the reference comparisons, including their exit-status checks."
    ));
    Ok(checks)
}

fn coordinator_probe(arg: &str) -> bool {
    use std::path::Component;
    let mut parts = Vec::new();
    for component in std::path::Path::new(arg).components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::ParentDir => { parts.pop(); }
            Component::RootDir | Component::Prefix(_) => parts.clear(),
            Component::CurDir => {}
        }
    }
    parts.windows(2).any(|pair| pair[0] == ".slag" && pair[1] == "checks")
}
