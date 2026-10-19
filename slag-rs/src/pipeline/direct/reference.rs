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

pub(super) fn checks(commission: &str, reference: Option<Reference>, launches: &[String]) -> Option<Checks> {
    let Some(tool) = named_tool(commission) else {
        return reference.is_none().then(Checks::default);
    };
    let reference = reference?;
    if reference.command.first()? != &tool || reference.actual.first()? == &tool { return None }
    for argv in [&reference.actual, &reference.command] {
        if argv.len() > 16 || !argv.iter().any(|arg| arg == "{input}")
            || argv.iter().any(|arg| arg.is_empty() || arg.len() > 256 || arg.chars().any(char::is_control))
            || crate::engine::agent::review_command_error(&shell_words::join(argv)).is_some()
        { return None; }
    }
    let input = reference.actual.iter().position(|arg| arg == "{input}")?;
    if input == 0 || !launches.iter().any(|launch| shell_words::split(launch).ok()
        .is_some_and(|argv| argv.starts_with(&reference.actual[..input])))
    { return None; }
    let mut checks = Checks::default();
    for (case, text) in [("empty", ""), ("no-final-newline", "alpha beta\nalpha")] {
        let path = format!("tests/fixtures/reference-{case}.txt");
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
    Some(checks)
}
