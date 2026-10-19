use serde::Deserialize;

mod reference;

const PACKAGE_CRITERION: &str = "Include installation metadata and importable modules for the requested package. Document its intended installation and launch or import commands.";
const CLI_CRITERION: &str = "Run the stated CLI launch. Use subprocess checks that assert actual results for the named behavior and flags.";
const TEST_CRITERION: &str = "Run a nonempty full test suite that exercises the commission's named behavior.";

#[derive(Debug, PartialEq, Eq)]
pub struct DirectPlan {
    pub proof: String,
    pub criteria: Vec<String>,
    pub commands: Vec<String>,
    pub fixtures: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeChoice {
    direct: bool,
    #[serde(default)]
    proof: String,
    #[serde(default)]
    criteria: Vec<String>,
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    reference: Option<reference::Reference>,
}

pub fn scope_prompt(commission: &str) -> String {
    let tool = reference::named_tool(commission);
    let mut example = serde_json::json!({"direct": true, "proof": "test command", "criteria": ["checkable requirement"], "commands": ["CLI probe"]});
    if let Some(tool) = &tool {
        example["reference"] = serde_json::json!({"actual": ["python3", "app.py", "{input}"], "command": [tool, "{input}"], "comparison": "words"});
    }
    let reference = tool.map(|tool| format!(
        "\nNamed reference tool: {tool}. Also return a reference object: \
         {{\"actual\":[\"python3\",\"app.py\",\"{{input}}\"],\"command\":[\"{tool}\",\"{{input}}\"],\"comparison\":\"words\"}}.\n\
         actual must use your chosen CLI launch. Substitute {{input}} for the input file in both argv arrays.\n\
         Choose comparison=bytes for exact output or words when only whitespace differs in the requested output.\n\
         The coordinator supplies immutable-content fixtures for empty input and an unterminated final text record.\n\
         Compare only the requested behavior using the reference tool's documented semantics. Include other classic cases in the suite."
    )).unwrap_or_default();
    format!(
        "Choose the work scope for a fresh, empty project.\n\
         COMMISSION: {commission}\n\
         Routing only: direct=true suits small local utilities, about four files, without external services, UI, deployment, or architectural research.\n\
         Return only {example} or {{\"direct\":false}}.\n\
         Write one short criterion per named requirement, targeting at most six. Keep routing and harness limits out of criteria.\n\
         State observable inputs/results, requested flags, package metadata/import shape, and intended launch method where relevant.\n\
         Match a named reference only for requested behavior, including input boundaries and byte versus character counts.\n\
         Add no unrequested flags, features, dependency bans, test quotas, defaults, ordering rules, or --help checks.\n\
         proof must run the full declared test suite without selection, skip-only, or test-name filters.\n\
         CLI/scripts need a normal launch probe with concrete fixture paths the builder will commit; exercise each named flag.\n\
         CLI probes must write no output files. Print results to stdout. Use /dev/stdout for a required output-path argument.\n\
         Prefer one probe, at most two. Non-CLI libraries may use commands=[].\n\
         Launch scripts with their interpreter unless an executable script is requested. Use the declared environment for installed packages.\n\
         Tests/probes must support filesystem-read-only execution through in-memory data or committed fixtures, without installs, fixture writes, or cache writes.\n\
         Return direct=false if this verification contract conflicts with the commission.\n\
         Criteria must be single lines of at most 320 characters; proof and each probe must fit 1024 characters.\n\
         Output the JSON itself without tools, prose, or document descriptions.{reference}"
    )
}

fn scope_json(raw: &str) -> Option<&str> {
    if raw.len() > 8_192 { return None }
    let raw = raw.trim();
    if let Some(body) = raw.strip_prefix("```json\n").or_else(|| raw.strip_prefix("```json\r\n")) {
        body.strip_suffix("\n```").map(str::trim)
    } else {
        Some(raw)
    }
}

pub fn scope_repair_prompt(commission: &str, previous: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(scope_json(previous)?).ok()?;
    if value.get("direct") != Some(&serde_json::Value::Bool(true)) { return None }
    Some(format!(
        "{}\n\nPREVIOUS SCOPE REPLY:\n{previous}\n\n\
         Your direct contract failed validation. Keep the named requirements.\n\
         proof must run the full declared test suite.\n\
         Concrete CLI probes must print to stdout. Do not use redirection, shell operators, installs, or no-op commands.\n\
         Include each named flag as an actual command argument.\n\
         Return only corrected JSON with direct, proof, criteria, commands, and any required reference object.",
        scope_prompt(commission),
    ))
}

pub fn reference_mapping_prompt(commission: &str, raw: &str) -> Option<String> {
    let scope: serde_json::Value = serde_json::from_str(scope_json(raw)?).ok()?;
    if scope["direct"] != true { return None }
    let tool = reference::named_tool(commission)?;
    Some(format!(
        "Map the requested behavior to the named reference tool.\nCOMMISSION: {commission}\n\
         PLANNED CLI PROBES: {}\n\
         Return ONLY this JSON object, with no scope fields: \
         {{\"actual\":[\"python3\",\"app.py\",\"{{input}}\"],\"command\":[\"{tool}\",\"{{input}}\"],\"comparison\":\"words\"}}.\n\
         actual must use the planned product CLI. command must invoke {tool} with the options for the requested behavior only.\n\
         Use {{input}} as a file argument in both arrays. Use documented reference semantics.\n\
         Choose comparison=bytes for exact text or comparison=words for whitespace-separated output fields.\n\
         These commands will compare empty input and an unterminated final text record. Do not use tools.",
        scope["commands"],
    ))
}

pub fn complete_reference(commission: &str, scope: &str, reply: &str) -> Option<DirectPlan> {
    let mut scope: serde_json::Value = serde_json::from_str(scope_json(scope)?).ok()?;
    let reply: serde_json::Value = serde_json::from_str(scope_json(reply)?).ok()?;
    scope["reference"] = reply.get("reference").filter(|value| !value.is_null()).unwrap_or(&reply).clone();
    parse_scope(&scope.to_string(), commission)
}

pub fn parse_scope(raw: &str, commission: &str) -> Option<DirectPlan> {
    let choice: ScopeChoice = serde_json::from_str(scope_json(raw)?).ok()?;
    if !choice.direct || choice.criteria.is_empty() || choice.criteria.len() > 12 { return None }
    let requested_node_tests = commission.to_ascii_lowercase().contains("node:test");
    let proof = if requested_node_tests { "node --test" } else { choice.proof.trim() };
    if proof.is_empty() || proof.len() > 1_024 || proof.chars().any(char::is_control)
        || matches!(proof, "true" | ":" | "exit 0")
        || write_redirection(proof) || crate::engine::agent::review_command_error(proof).is_some()
        || shell_words::split(proof).ok()?.is_empty()
    {
        return None;
    }
    let mut criteria: Vec<String> = Vec::new();
    for item in choice.criteria {
        let item = item.trim();
        if item.is_empty() || item.chars().count() > 320 || item.chars().any(char::is_control) {
            return None;
        }
        if !criteria.iter().any(|previous| previous.eq_ignore_ascii_case(item)) {
            criteria.push(item.to_string());
        }
    }
    if choice.commands.len() > 4 { return None }
    let mut commands = Vec::new();
    let mut tokens = Vec::new();
    for command in choice.commands {
        let command = command.trim();
        if command.is_empty() || command.len() > 1_024 || command.chars().any(char::is_control)
            || matches!(command, "true" | "false" | ":" | "exit 0") || write_redirection(command)
            || crate::engine::agent::review_command_error(command).is_some()
        {
            return None;
        }
        let words = shell_words::split(command).ok()?;
        if words.is_empty() { return None }
        if words.first().is_some_and(|word| matches!(word.as_str(), "echo" | "printf"))
            && !words.iter().any(|word| matches!(word.as_str(), "|" | "&&" | ";"))
        {
            return None;
        }
        tokens.extend(words);
        if !commands.iter().any(|previous| previous == command) {
            commands.push(command.to_string());
        }
    }
    let lower = commission.to_lowercase();
    let cli = regex::Regex::new(r"\b(cli|command[- ]line|script|executable)\b").ok()?;
    let script = regex::Regex::new(r"\b[\w.-]+\.(py|js|mjs|cjs|ts|sh|rb|pl|ps1)\b").ok()?;
    let library = regex::Regex::new(r"\b(library|module|function|api)\b").ok()?;
    let needs_cli = cli.is_match(&lower) || script.is_match(&lower) && !library.is_match(&lower);
    if commands.is_empty() && needs_cli {
        return None;
    }
    let flag = regex::Regex::new(r#"(?:^|[\s`'"(,;/])(-{1,2}[A-Za-z][A-Za-z0-9_-]*)"#).ok()?;
    for stated in flag.captures_iter(commission) {
        let named = &stated[1];
        if !criteria.iter().any(|item| flag.captures_iter(item).any(|found| &found[1] == named))
            || !tokens.iter().any(|token| token == named || token.strip_prefix(named).is_some_and(|rest| rest.starts_with('=')))
        {
            return None;
        }
    }
    let package = regex::Regex::new(r"\bpackages?\b").ok()?.is_match(&lower);
    let tests = regex::Regex::new(r"\b(tests?|unittests?|pytest|node:test)\b").ok()?.is_match(&lower);
    for (required, criterion) in [(package, PACKAGE_CRITERION), (needs_cli, CLI_CRITERION), (tests, TEST_CRITERION)] {
        if required && !criteria.iter().any(|existing| existing == criterion) {
            criteria.push(criterion.to_string());
        }
    }
    let reference = reference::checks(commission, choice.reference, &commands)?;
    if let Some(criterion) = reference.criterion { criteria.push(criterion); }
    commands.extend(reference.commands);
    Some(DirectPlan { proof: proof.to_string(), criteria, commands, fixtures: reference.fixtures })
}

fn write_redirection(command: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if escaped { escaped = false; continue }
        if c == '\\' && quote != Some('\'') { escaped = true; continue }
        if quote == Some(c) { quote = None; continue }
        if quote.is_none() {
            if matches!(c, '\'' | '"') { quote = Some(c); }
            else if c == '>' {
                if chars.peek().is_some_and(|next| matches!(*next, '>' | '|' | '&')) { return true }
                while chars.peek().is_some_and(|next| next.is_whitespace()) { chars.next(); }
                let mut target = String::new();
                let mut target_quote = None;
                let mut target_escaped = false;
                while let Some(&next) = chars.peek() {
                    if target_quote.is_none() && !target_escaped
                        && (next.is_whitespace() || ";|&()<>".contains(next))
                    {
                        break;
                    }
                    chars.next();
                    target.push(next);
                    if target_escaped { target_escaped = false; continue }
                    if next == '\\' && target_quote != Some('\'') { target_escaped = true; continue }
                    if target_quote == Some(next) { target_quote = None; }
                    else if target_quote.is_none() && matches!(next, '\'' | '"') { target_quote = Some(next); }
                }
                let Ok(words) = shell_words::split(&target) else { return true };
                if words.len() != 1 || !matches!(words[0].as_str(), "/dev/stdout" | "/dev/stderr" | "/dev/null") {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(criteria: serde_json::Value) -> String {
        answer_with_commands(criteria, &[])
    }

    fn answer_with_commands(criteria: serde_json::Value, commands: &[&str]) -> String {
        serde_json::json!({"direct": true, "proof": "python3 -m unittest", "criteria": criteria, "commands": commands}).to_string()
    }

    #[test]
    fn a_direct_plan_keeps_named_behaviors_flags_and_delivery_requirements() {
        let commission = "Create an importable tally package with a CLI: python3 -m tally FILE. Add --lines and -q, with unittest.";
        let raw = answer_with_commands(serde_json::json!([
            "The tally package includes metadata for its intended distribution and supports import tally.",
            "python3 -m tally FILE prints the count for the supplied file.",
            "Subprocess CLI tests show --lines counts lines and -q suppresses labels.",
            "unittest checks the requested counts with committed fixtures or in-memory data."
        ]), &["python3 -m tally tests/fixtures/input.txt --lines -q"]);
        let plan = parse_scope(&raw, commission).unwrap();
        assert_eq!(plan.proof, "python3 -m unittest");
        assert_eq!(plan.criteria.len(), 7);
        assert!(plan.criteria[2].contains("--lines"));
        assert_eq!(plan.commands, ["python3 -m tally tests/fixtures/input.txt --lines -q"]);
    }

    #[test]
    fn missing_named_flags_cannot_clear_the_direct_route() {
        let raw = answer_with_commands(serde_json::json!(["Subprocess CLI tests cover --lines-extra."]),
            &["python3 count.py tests/fixtures/input.txt --lines-extra"]);
        assert!(parse_scope(&raw, "Support --lines.").is_none());
        assert!(parse_scope(&raw, "Support --lines-extra and -q.").is_none());
        assert!(parse_scope(&raw, "Support --lines-extra.").is_some());
        assert!(parse_scope(&answer(serde_json::json!(["Check word-count behavior."])), "A word-count utility.").is_some());
    }

    #[test]
    fn cli_probes_must_exercise_named_flags_as_command_arguments() {
        let criteria = serde_json::json!(["Subprocess CLI tests show --pretty formats the requested output."]);
        let fake = answer_with_commands(criteria.clone(), &["python3 -c 'print(\"--pretty\")'"]);
        assert!(parse_scope(&fake, "Build a CLI with --pretty.").is_none());
        let real = answer_with_commands(criteria, &["python3 -m tally tests/fixtures/input.txt --pretty=json"]);
        assert!(parse_scope(&real, "Build a CLI with --pretty.").is_some());
        let criteria = serde_json::json!(["The script counts words from the supplied file."]);
        let commission = "write wc.py that counts words in a file, with a unittest";
        assert!(parse_scope(&answer(criteria.clone()), commission).is_none());
        assert!(parse_scope(&answer_with_commands(criteria, &["python3 wc.py tests/fixtures/words.txt"]), commission).is_some());
    }

    #[test]
    fn probe_commands_reject_obvious_mutations_installs_noops_and_overflow() {
        for commands in [
            vec!["true"], vec!["echo done"], vec!["printf done"],
            vec!["python3 tally.py fixtures/input.txt > output.txt"],
            vec!["touch fixture.txt"], vec!["python3 -m pip install tally"],
            vec!["python3 tally.py '"],
            vec!["python3 tally.py fixtures/input.txt"; 5],
        ] {
            let raw = answer_with_commands(serde_json::json!(["The CLI counts the supplied input."]), &commands);
            assert!(parse_scope(&raw, "Create a counting CLI.").is_none(), "accepted {commands:?}");
        }
        assert!(!write_redirection("python3 -c 'assert 3 > 2'"));
        assert!(write_redirection("python3 tally.py fixture.txt 2> errors.txt"));
    }

    #[test]
    fn device_redirections_require_exact_literal_targets() {
        for command in ["python3 cli.py > /dev/stdout", "python3 cli.py 2>/dev/stderr", "python3 cli.py > '/dev/null'",
            "python3 cli.py > /dev/stdout; python3 cli.py 2> /dev/null"]
        {
            assert!(!write_redirection(command), "rejected {command}");
            assert!(parse_scope(&answer_with_commands(serde_json::json!(["The CLI counts input."]), &[command]), "Create a CLI.").is_some());
        }
        for command in ["python3 cli.py > /dev/stdout.txt", "python3 cli.py > /dev/null/../output.txt",
            "python3 cli.py > '/dev/stdout'output", "python3 cli.py > $OUTPUT", "python3 cli.py >| /dev/stdout",
            "python3 cli.py >> /dev/null", "python3 cli.py > /dev/stdout > product.txt"]
        {
            assert!(write_redirection(command), "accepted {command}");
        }
    }

    #[test]
    fn invalid_or_unbounded_scope_documents_fall_back_to_planning() {
        for raw in [
            r#"{"direct":false}"#.to_string(),
            r#"{"direct":true,"proof":"python3 -m unittest"}"#.to_string(),
            r#"{"direct":true,"proof":"true","criteria":["Counts words."]}"#.to_string(),
            r#"{"direct":true,"proof":":","criteria":["Counts words."]}"#.to_string(),
            r#"{"direct":"true","proof":"python3 -m unittest","criteria":["Counts words."]}"#.to_string(),
            answer(serde_json::json!([])),
            answer(serde_json::json!([""])),
            answer(serde_json::json!(["A condition.\n- Another condition."])),
            answer(serde_json::json!(["x".repeat(321)])),
            answer(serde_json::json!(vec!["Counts words."; 13])),
            answer(serde_json::json!([false])),
            "A small project.".to_string(),
        ] {
            assert!(parse_scope(&raw, "Count words with unittest.").is_none(), "accepted {raw}");
        }
    }

    #[test]
    fn an_explicit_test_framework_controls_the_suite_command() {
        let raw = answer_with_commands(serde_json::json!(["node:test exercises CLI counts."]),
            &["node cli.js fixtures/input.txt 2"]);
        let plan = parse_scope(&raw, "A Node CLI with node:test tests.").unwrap();
        assert_eq!(plan.proof, "node --test");
    }

    #[test]
    fn duplicate_criteria_do_not_expand_the_contract() {
        let plan = parse_scope(&answer(serde_json::json!([" Counts words. ", "counts words."])), "Count words.").unwrap();
        assert_eq!(plan.criteria, ["Counts words."]);
    }

    #[test]
    fn named_package_cli_and_tests_get_contracts_when_the_model_omits_them() {
        let commission = "Create a package with a CLI and unittest for CSV totals.";
        let commands = ["python3 -m totals tests/fixtures/input.csv"];
        let plan = parse_scope(&answer_with_commands(serde_json::json!(["CSV totals print the requested result."]), &commands), commission).unwrap();
        assert_eq!(plan.criteria.len(), 4);
        assert!(plan.criteria.iter().any(|item| item.contains("installation metadata") && item.contains("importable modules")));
        assert!(plan.criteria.iter().any(|item| item.contains("subprocess checks") && item.contains("assert actual results")));
        assert!(plan.criteria.iter().any(|item| item.contains("nonempty full test suite") && item.contains("named behavior")));
        let repeated = parse_scope(&answer_with_commands(serde_json::json!(plan.criteria), &commands), commission).unwrap();
        assert_eq!(repeated.criteria.len(), 4);
    }

    #[tokio::test]
    async fn reference_probes_detect_unterminated_record_errors_without_tool_specific_rules() {
        let raw = serde_json::json!({
            "direct": true, "proof": "python3 -m unittest", "criteria": ["Copy file contents like cat."],
            "commands": ["python3 copy.py tests/fixtures/sample.txt"],
            "reference": {"actual": ["python3", "copy.py", "{input}"], "command": ["cat", "{input}"], "comparison": "bytes"}
        }).to_string();
        let plan = parse_scope(&raw, "A file CLI like cat with tests.").unwrap();
        assert_eq!(plan.commands.len(), 3);
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("tests/fixtures")).unwrap();
        std::fs::write(root.path().join("tests/fixtures/reference-empty.txt"), "").unwrap();
        std::fs::write(root.path().join("tests/fixtures/reference-no-final-newline.txt"), "alpha beta\nalpha").unwrap();
        let script = root.path().join("copy.py");
        let sandbox = crate::engine::sandbox::Sandbox::read_only().unwrap();
        for (source, expected) in [
            ("import pathlib,sys\nsys.stdout.buffer.write(pathlib.Path(sys.argv[1]).read_bytes())\n", [true, true]),
            ("import pathlib,sys\nb=pathlib.Path(sys.argv[1]).read_bytes()\nsys.stdout.buffer.write(b + (b'\\n' if b and not b.endswith(b'\\n') else b''))\n", [true, false]),
            ("import pathlib,sys\nsys.stdout.buffer.write(pathlib.Path(sys.argv[1]).read_bytes())\nsys.exit(2)\n", [false, false]),
        ] {
            std::fs::write(&script, source).unwrap();
            for (command, expected) in plan.commands[1..].iter().zip(expected) {
                let result = sandbox.wrap("bash").args(["-c", command]).current_dir(root.path()).output().await.unwrap();
                assert_eq!(result.status.success(), expected, "{}", String::from_utf8_lossy(&result.stderr));
            }
        }
        std::fs::write(&script, "import pathlib,sys\nsys.stdout.buffer.write(pathlib.Path(sys.argv[1]).read_bytes())\n").unwrap();
        std::fs::write(root.path().join("tests/fixtures/reference-no-final-newline.txt"), "alpha beta\nalpha\n").unwrap();
        let result = sandbox.wrap("bash").args(["-c", &plan.commands[2]]).current_dir(root.path()).output().await.unwrap();
        assert!(!result.status.success(), "changed boundary input must not pass");
        assert!(String::from_utf8_lossy(&result.stderr).contains("reference fixture changed"));
    }

    #[tokio::test]
    async fn reference_count_fields_reject_an_unterminated_line_mismatch() {
        let raw = serde_json::json!({
            "direct": true, "proof": "python3 -m unittest", "criteria": ["Count lines, words and bytes like wc."],
            "commands": ["python3 wc.py fixtures/empty.txt", "python3 wc.py fixtures/normal.txt", "python3 wc.py fixtures/unterminated.txt"],
            "reference": {"actual": ["python3", "wc.py", "{input}"], "command": ["wc", "{input}"], "comparison": "words"}
        }).to_string();
        let plan = parse_scope(&raw, "Write wc.py that counts lines, words and bytes like wc, with unittest.").unwrap();
        let root = tempfile::tempdir().unwrap();
        for (path, content) in &plan.fixtures {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let sandbox = crate::engine::sandbox::Sandbox::read_only().unwrap();
        for (lines, expected) in [("len(b.splitlines())", [true, false]), ("b.count(bytes([10]))", [true, true])] {
            let source = format!("import pathlib,sys\nb=pathlib.Path(sys.argv[1]).read_bytes()\nprint({lines},len(b.split()),len(b),sys.argv[1])\n");
            std::fs::write(root.path().join("wc.py"), source).unwrap();
            for (command, expected) in plan.commands[3..].iter().zip(expected) {
                let result = sandbox.wrap("bash").args(["-c", command]).current_dir(root.path()).output().await.unwrap();
                assert_eq!(result.status.success(), expected, "{}", String::from_utf8_lossy(&result.stderr));
            }
        }
    }

    #[test]
    fn a_named_reference_requires_the_actual_reference_and_artifact_launch() {
        let mut raw = serde_json::json!({
            "direct": true, "proof": "python3 -m unittest", "criteria": ["Copy file contents like cat."],
            "commands": ["python3 copy.py tests/fixtures/sample.txt"]
        });
        assert!(parse_scope(&raw.to_string(), "A file CLI like cat with tests.").is_none());
        let mut complex = raw.clone();
        complex["commands"] = serde_json::json!(["python3 copy.py --mode raw tests/fixtures/sample.txt"]);
        assert!(parse_scope(&complex.to_string(), "A file CLI like cat with tests.").is_none());
        raw["reference"] = serde_json::json!({"actual": ["python3", "copy.py", "{input}"], "command": ["cat", "{input}"], "comparison": "bytes"});
        assert!(parse_scope(&raw.to_string(), "A file CLI like cat with tests.").is_some());
        assert!(parse_scope(&raw.to_string(), "A file CLI with tests.").is_none());
        for (key, value) in [
            ("command", serde_json::json!(["echo", "{input}"])),
            ("actual", serde_json::json!(["cat", "{input}"])),
            ("actual", serde_json::json!(["python3", "other.py", "{input}"])),
            ("actual", serde_json::json!(["python3", "copy.py", "unrelated.txt"])),
            ("comparison", serde_json::json!("ignore errors")),
        ] {
            let mut invalid = raw.clone(); invalid["reference"][key] = value;
            assert!(parse_scope(&invalid.to_string(), "A file CLI like cat with tests.").is_none(), "{invalid}");
        }
    }

    #[test]
    fn an_exact_outer_json_fence_preserves_the_scope_contract() {
        let json = answer(serde_json::json!(["Counts words."]));
        let expected = parse_scope(&json, "Count words.").unwrap();
        for fenced in [format!("```json\n{json}\n```"), format!(" \n```json\r\n{json}\r\n```\n ")] {
            assert_eq!(parse_scope(&fenced, "Count words."), Some(DirectPlan {
                proof: expected.proof.clone(), criteria: expected.criteria.clone(), commands: expected.commands.clone(), fixtures: expected.fixtures.clone(),
            }));
        }
        for malformed in [
            format!("A plan:\n```json\n{json}\n```"),
            format!("```json\n{json}\n```\nReady."),
            format!("```json\n{json}"),
            format!("```\n{json}\n```"),
            format!("```json\n{json}\n```\n```json\n{json}\n```"),
        ] {
            assert!(parse_scope(&malformed, "Count words.").is_none());
        }
    }
}
