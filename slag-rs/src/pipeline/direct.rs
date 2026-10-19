use serde::Deserialize;

mod reference;

const PACKAGE_CRITERION: &str = "Include installation metadata and importable modules for the requested package. Document its intended installation and launch or import commands.";
const CLI_CRITERION: &str = "Judge named behavior and boundary cases before interface conveniences. Use subprocess checks with exact expected results for the named behavior and flags. Run the stated CLI launch and --help.";
const TEST_CRITERION: &str = "Run a nonempty full test suite that exercises the commission's named behavior. Its documented command must pass in a fresh clone without prior installation.";

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
         Routing only: direct=true suits small local utilities with a clear local interface, without external services, UI, deployment, or architectural research.\n\
         Return only {example} or {{\"direct\":false}}.\n\
         List primary behavior criteria first, then a few secondary interface criteria. Target six short criteria. Keep routing and harness limits out of criteria.\n\
         Derive observable inputs/results from the standard meaning of the named operations and formats. Give each primary behavior an edge-case assertion in the suite.\n\
         Examples apply only to named operations. Tail selects the last N lines (default 10); test -n 0, -n +N, appends, and partial records.\n\
         A regex filter searches each complete line; invalid patterns fail with a useful error. A wc-like tool includes -l/-w/-c and combined flags.\n\
         CSV conversion handles UTF-8 BOM and quoted newlines; reject ragged rows with a useful error instead of silent data loss. Unicode word frequency treats canonically equivalent words alike through NFC.\n\
         Process potentially unbounded file/text streams incrementally. Do not replace tail or follow with whole-file reads. Test follow with appended data and partial lines.\n\
         Include those behaviors only when the sentence names that operation or format. Do not add log rotation, network services, or unrelated features.\n\
         Match a named reference only for requested behavior, including input boundaries and byte versus character counts.\n\
         Keep conventional interfaces secondary. Do not invent --file, --input, or --regex aliases when positional arguments suffice. Add no unrelated features, dependency bans, test quotas, or output-order rules.\n\
         proof must run the full declared test suite from a fresh clone without prior installation or environment activation. Use the system interpreter, not .venv. Do not select, skip, or filter tests.\n\
         CLI/scripts need a normal launch probe; exercise each named flag. Keep harness fixtures and probe scripts under .slag/checks/.\n\
         CLI probes must write no workspace output files. Print results to stdout. Use private TMPDIR for scratch. Put probe scripts under .slag/checks/, outside product tests.\n\
         For long-running flags such as --follow, pass the flag to a probe script under .slag/checks/. It must assert child results, enforce a timeout, and terminate the child.\n\
         Prefer probes that assert primary semantics and a named flag in the same run. Derive secondary interfaces from this tool's kind. Allow up to six direct probes. Non-CLI libraries may use commands=[].\n\
         A Python package CLI needs installation metadata with a console-script entry point and __main__.py. Probe python3 -m PACKAGE from source. The coordinator verifies installation and the console script in an isolated copy. Never install into the product workspace. The suite uses source or module launches.\n\
         A Node CLI needs package.json with a bin entry. Probe that bin target.\n\
         A Unix stream filter, such as wc, tail, CSV conversion, or word frequency, reads stdin without a file or with literal -. A scalar parser, such as duration parsing, takes its value positionally; do not add file or stdin interfaces. CLIs support --help. A top-N CLI has a documented default N. Test FILE N and the default; do not require an extra flag unless the sentence names it.\n\
         Test these conventional interfaces with subprocess assertions. Match any explicit interface restrictions in the commission.\n\
         Launch standalone scripts with their interpreter. Keep each verification command within the declared environment.\n\
         The workspace stays read-only during tests/probes. Use in-memory data, .slag/checks/ fixtures, or private TMPDIR scratch. Product tests must not depend on .slag/. Do not install dependencies or write workspace fixtures or caches.\n\
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

pub fn reference_validation_error(commission: &str, raw: &str) -> Option<String> {
    reference::named_tool(commission)?;
    let value: serde_json::Value = serde_json::from_str(scope_json(raw)?).ok()?;
    let reference = value.get("reference").unwrap_or(&value);
    let reference = if reference.is_null() || value.get("direct").is_some() && value.get("reference").is_none() {
        None
    } else {
        match serde_json::from_value(reference.clone()) {
            Ok(reference) => Some(reference),
            Err(error) => return Some(format!("the reference mapping has an invalid JSON shape: {error}")),
        }
    };
    reference::checks(commission, reference).err().map(str::to_string)
}

pub fn reference_repair_prompt(commission: &str, scope: &str, reply: &str) -> Option<String> {
    let reason = reference_validation_error(commission, reply)?;
    Some(format!("{}\n\nPREVIOUS REFERENCE REPLY:\n{reply}\n\nVALIDATION ERROR: {reason}.\nReturn one corrected reference object. The product command may differ from a probe wrapper.",
        reference_mapping_prompt(commission, scope)?))
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
        || shell_words::split(proof).ok()?.is_empty() || proof.contains(".venv/")
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
    if choice.commands.len() > 6 { return None }
    let mut commands = Vec::new();
    let mut tokens = Vec::new();
    for command in choice.commands {
        let command = command.trim();
        if command.is_empty() || command.len() > 1_024 || command.chars().any(char::is_control)
            || matches!(command, "true" | "false" | ":" | "exit 0") || write_redirection(command) || command.contains(".venv/")
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
    let conventions = conventional_criteria(commission, needs_cli, package);
    for criterion in conventions {
        if !criteria.iter().any(|existing| existing == criterion) { criteria.push(criterion.into()); }
    }
    let reference = reference::checks(commission, choice.reference).ok()?;
    if let Some(criterion) = reference.criterion { criteria.push(criterion); }
    commands.extend(reference.commands);
    Some(DirectPlan { proof: proof.to_string(), criteria, commands, fixtures: reference.fixtures })
}

fn python_commission(commission: &str) -> bool {
    let lower = commission.to_ascii_lowercase();
    ["python", "unittest", ".py"].iter().any(|word| lower.contains(word))
}

fn stream_filter(commission: &str) -> bool {
    let lower = commission.to_ascii_lowercase();
    let operation = regex::Regex::new(r"\b(wc|tail|tails|tailing)\b").unwrap();
    let converts = regex::Regex::new(r"\b(convert|converts|converter|conversion)\b").unwrap();
    let words = regex::Regex::new(r"\bwords?\b").unwrap();
    operation.is_match(&lower)
        || lower.contains("csv") && converts.is_match(&lower)
        || words.is_match(&lower) && ["frequent", "frequency", "top n", "top-n"].iter().any(|term| lower.contains(term))
}

fn conventional_criteria(commission: &str, cli: bool, package: bool) -> Vec<&'static str> {
    if !cli { return Vec::new(); }
    let lower = commission.to_ascii_lowercase();
    let mut criteria = Vec::new();
    if package && python_commission(commission) {
        criteria.push("Provide a console-script entry point and __main__.py for python3 -m PACKAGE. Run the module from source; verify installation and the console script in an isolated copy. The suite must pass from a fresh clone without installation.");
    }
    if lower.contains("node") {
        criteria.push("Provide package.json with a bin entry that points to the runnable CLI. Document and verify that entry.");
    }
    if stream_filter(commission) {
        criteria.push("Read stdin when no input file is given or the input file is literal -. Document both interfaces and --help. Run stdin, dash, and help probes. Test their results through the real CLI.");
    }
    if lower.contains("top n") || lower.contains("top-n") {
        criteria.push("Choose and document a sensible default N. Accept FILE N positionally. Verify the default and positional forms through CLI tests; require a flag only when the commission names it.");
    }
    criteria
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

    #[test]
    fn conventional_interfaces_follow_the_requested_deliverable() {
        let python = conventional_criteria("a Python package CLI that converts CSV files to JSON", true, true);
        assert!(python.iter().any(|item| item.contains("console-script")));
        assert!(python.iter().any(|item| item.contains("stdin")));
        let node = conventional_criteria("a tiny Node CLI for top N words of a text file", true, false);
        assert!(node.iter().any(|item| item.contains("bin entry")));
        assert!(node.iter().any(|item| item.contains("default N")));
        assert!(!node.iter().any(|item| item.contains("console-script")));
        assert!(conventional_criteria("a Python package library", false, true).is_empty());
        assert!(conventional_criteria("a CLI that shows the date", true, false).is_empty());
    }

    #[test]
    fn scalar_and_file_tools_do_not_inherit_stream_filter_interfaces() {
        for commission in [
            "a small Python library and CLI that parses durations like 1h30m or 90s into seconds, with tests",
            "a CLI that inspects a file and prints its metadata",
            "a CLI that renders text like a typewriter",
            "a CLI that validates a CSV schema file",
        ] {
            let criteria = conventional_criteria(commission, true, false);
            assert!(!criteria.iter().any(|item| item.contains("stdin")), "{commission}");
        }
        for commission in [
            "write wc.py that counts lines, words and bytes of a file like wc",
            "a Python CLI that tails a log file and prints only lines matching a regex",
            "a small Python CLI that converts CSV to JSON",
            "a tiny Node CLI that prints the top N most frequent words of a text file",
        ] {
            assert!(stream_filter(commission), "{commission}");
        }
        let prompt = scope_prompt("a Python CLI that parses durations");
        assert!(prompt.contains("takes its value positionally"));
        assert!(prompt.contains("do not add file or stdin interfaces"));
    }

    #[test]
    fn scope_keeps_harness_instruments_out_of_product_tests() {
        let prompt = scope_prompt("a Python CLI with tests");
        assert!(prompt.contains("Keep harness fixtures and probe scripts under .slag/checks/"));
        assert!(prompt.contains("Product tests must not depend on .slag/"));
        assert!(!prompt.contains("committed probe script"));
    }

    #[test]
    fn scope_prioritizes_semantics_and_limits_convenience_interfaces() {
        let prompt = scope_prompt("a Python CLI that tails a log file with --follow and tests");
        let semantics = prompt.find("List primary behavior criteria first").unwrap();
        let interfaces = prompt.find("Keep conventional interfaces secondary").unwrap();
        assert!(semantics < interfaces);
        for requirement in ["last N lines", "partial records", "-n 0", "-n +N", "-l/-w/-c", "UTF-8 BOM",
            "quoted newlines", "ragged rows", "NFC", "incrementally", "edge-case assertion"]
        {
            assert!(prompt.contains(requirement), "missing {requirement}");
        }
        assert!(prompt.contains("only when the sentence names"));
        assert!(prompt.contains("Do not invent --file, --input, or --regex aliases"));
        assert!(!prompt.contains("Probe both FILE N and FILE --top N"));
        assert!(!prompt.contains("python3 -c with subprocess.run"));
    }

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
        assert_eq!(plan.criteria.len(), 8);
        assert!(plan.criteria[2].contains("--lines"));
        assert_eq!(plan.commands, ["python3 -m tally tests/fixtures/input.txt --lines -q"]);
    }

    #[test]
    fn package_probes_use_source_without_a_workspace_install() {
        let raw = answer_with_commands(serde_json::json!(["CSV conversion supports --pretty."]),
            &["python3 -m csvjson --pretty tests/fixtures/input.csv"]);
        let plan = parse_scope(&raw, "A Python package CLI for CSV with --pretty and tests.").unwrap();
        assert_eq!(plan.proof, "python3 -m unittest");
        assert_eq!(plan.commands, ["python3 -m csvjson --pretty tests/fixtures/input.csv"]);
        assert!(plan.criteria.iter().any(|item| item.contains("isolated copy")));
        assert!(!scope_prompt("A Python package CLI").contains("builder installs into .venv"));
    }

    #[test]
    fn fresh_clone_suite_rejects_an_installed_environment() {
        for proof in [".venv/bin/python -m unittest", "PATH=\"$PWD/.venv/bin:$PATH\" python3 -m unittest"] {
            let raw = serde_json::json!({"direct": true, "proof": proof,
                "criteria": ["Tests cover behavior."], "commands": []}).to_string();
            assert!(parse_scope(&raw, "A Python package with tests.").is_none(), "{proof}");
        }
        let plan = parse_scope(&answer(serde_json::json!(["Tests cover behavior."])), "A Python package with tests.").unwrap();
        assert_eq!(plan.proof, "python3 -m unittest");
        assert!(plan.criteria.iter().any(|item| item.contains("fresh clone without prior installation")));
    }

    #[test]
    fn first_review_conventions_cover_module_dash_and_positional_launches() {
        let python = conventional_criteria("A Python package CLI that converts CSV files to JSON", true, true);
        assert!(python.iter().any(|item| item.contains("__main__.py") && item.contains("python3 -m PACKAGE")));
        assert!(python.iter().any(|item| item.contains("literal -")));
        let node = conventional_criteria("A Node CLI for top N words of a text file", true, false);
        assert!(node.iter().any(|item| item.contains("FILE N positionally") && item.contains("only when the commission names it")));
        let prompt = scope_prompt("A Python package CLI that converts CSV files to JSON");
        assert!(prompt.contains("fresh clone without prior installation"));
        assert!(prompt.contains("suite uses source or module launches"));
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
            vec!["touch fixture.txt"], vec!["python3 -m pip install tally"], vec![".venv/bin/tally fixture.txt"],
            vec!["python3 tally.py '"],
            vec!["python3 tally.py fixtures/input.txt"; 7],
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
        assert_eq!(plan.criteria.len(), 5);
        assert!(plan.criteria.iter().any(|item| item.contains("installation metadata") && item.contains("importable modules")));
        assert!(plan.criteria.iter().any(|item| item.contains("subprocess checks") && item.contains("exact expected results")));
        assert!(plan.criteria.iter().any(|item| item.contains("nonempty full test suite") && item.contains("named behavior")));
        let repeated = parse_scope(&answer_with_commands(serde_json::json!(plan.criteria), &commands), commission).unwrap();
        assert_eq!(repeated.criteria.len(), 5);
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
        std::fs::create_dir_all(root.path().join(".slag/checks")).unwrap();
        std::fs::write(root.path().join(".slag/checks/reference-empty.txt"), "").unwrap();
        std::fs::write(root.path().join(".slag/checks/reference-no-final-newline.txt"), "alpha beta\nalpha").unwrap();
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
        std::fs::write(root.path().join(".slag/checks/reference-no-final-newline.txt"), "alpha beta\nalpha\n").unwrap();
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
    fn a_reference_product_launch_does_not_need_to_match_a_probe_wrapper() {
        let raw = serde_json::json!({
            "direct": true, "proof": "python3 -m unittest", "criteria": ["Count lines, words and bytes like wc."],
            "commands": ["python3 -B tests/cli_probe.py wc.py fixture.txt"],
            "reference": {"actual": ["python3", "-B", "wc.py", "{input}"], "command": ["wc", "{input}"], "comparison": "words"}
        }).to_string();
        let plan = parse_scope(&raw, "Write wc.py like wc with unittest.").unwrap();
        assert_eq!(plan.fixtures.len(), 2);
        assert!(plan.fixtures.iter().all(|(path, _)| path.starts_with(".slag/checks/reference-")));
        assert!(reference_validation_error("Write wc.py like wc with unittest.", &raw).is_none());
    }

    #[test]
    fn reference_product_paths_cannot_hide_coordinator_probes() {
        let mut raw = serde_json::json!({
            "direct": true, "proof": "python3 -m unittest", "criteria": ["Copy contents like cat."],
            "commands": ["python3 copy.py fixture.txt"],
            "reference": {"actual": ["python3", "copy.py", "{input}"], "command": ["cat", "{input}"], "comparison": "bytes"}
        });
        for path in [
            ".slag/checks/probe.py", "./.slag/checks/probe.py", "/tmp/project/.slag/checks/probe.py",
            "helpers/../.slag/checks/probe.py", "./.slag/helpers/../checks/probe.py",
            "../project/.slag/checks/./probe.py",
        ] {
            raw["reference"]["actual"][1] = serde_json::json!(path);
            let reply = raw.to_string();
            assert!(parse_scope(&reply, "A CLI like cat with tests.").is_none(), "{path}");
            assert!(reference_validation_error("A CLI like cat with tests.", &reply).unwrap().contains("coordinator probe"));
        }
        for path in ["./copy.py", "helpers/../copy.py", "/tmp/project/copy.py", ".slag/checks/../../copy.py"] {
            raw["reference"]["actual"][1] = serde_json::json!(path);
            assert!(parse_scope(&raw.to_string(), "A CLI like cat with tests.").is_some(), "{path}");
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
            ("actual", serde_json::json!(["python3", ".slag/checks/probe.py", "{input}"])),
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
