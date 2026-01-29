use crate::config::{ALLOY_FILE, BLUEPRINT, CRUCIBLE, HIGH_GRADE, LEDGER};
use crate::sexp::Ingot;

/// Build the prompt (flux) for striking an ingot.
/// Includes blueprint, alloy recipes, crucible state, ledger, git diff.
pub fn prepare_flux(ingot: &Ingot, slag: Option<&str>) -> String {
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "None".into());
    let alloy = std::fs::read_to_string(ALLOY_FILE).unwrap_or_else(|_| "None yet".into());
    let crucible = std::fs::read_to_string(CRUCIBLE).unwrap_or_else(|_| "Empty".into());
    let ledger = read_tail(LEDGER, 25);
    let git_diff = git_diff_stat();

    let complex_note = if ingot.grade >= HIGH_GRADE {
        " ◉ COMPLEX"
    } else {
        ""
    };
    let skill_note = if ingot.is_web() {
        " (Playwright available)"
    } else {
        ""
    };

    let mut flux = format!(
        "=== FORGE ORDER ===\n\
        [{id}] {work}\n\
        Grade: {grade}{complex_note}\n\
        Skill: {skill}{skill_note}\n\
        Heat: {heat}/{max}\n\
        Proof: {proof}\n\
        \n\
        === BLUEPRINT ===\n\
        {blueprint}\n\
        \n\
        === ALLOY RECIPES ===\n\
        {alloy}\n\
        \n\
        === CRUCIBLE STATE ===\n\
        {crucible}\n\
        \n\
        === RECENT LEDGER ===\n\
        {ledger}\n\
        \n\
        === GIT DIFF ===\n\
        {git_diff}\n\n",
        id = ingot.id,
        work = ingot.work,
        grade = ingot.grade,
        skill = ingot.skill,
        heat = ingot.heat,
        max = ingot.max,
        proof = ingot.proof,
    );

    if let Some(slag_msg) = slag {
        flux.push_str(&format!(
            "!!! CRACKED - PREVIOUS ATTEMPT FAILED !!!\n{slag_msg}\n!!! ANALYZE AND FIX !!!\n"
        ));
    } else {
        flux.push_str("=== INSTRUCTIONS ===\n");
        flux.push_str("1. Forge this ingot completely\n");
        flux.push_str("2. Create/modify all necessary files\n");
        flux.push_str("3. Add useful patterns to AGENTS.md\n");
        flux.push_str("4. End with exactly: CMD: <shell command to verify>\n\n");

        if ingot.is_complex() {
            flux.push_str("◉ COMPLEX - think through edge cases\n");
        }
        if ingot.is_web() {
            flux.push_str("◉ WEB SKILL - Playwright available for browser testing\n");
        }

        flux.push_str(
            "\nRULES:\n\
            - NO QUESTIONS. You are the expert.\n\
            - NO PROSE. Just code and CMD.\n\
            - The CMD must pass for the ingot to be forged.\n",
        );
    }

    flux
}

/// Build the re-smelt analysis prompt for a cracked ingot
pub fn prepare_resmelt_flux(ingot: &Ingot, failure_logs: &str) -> String {
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "None".into());
    let crucible = std::fs::read_to_string(CRUCIBLE).unwrap_or_else(|_| "Empty".into());
    let git_state = git_log_and_diff();

    format!(
        "=== RE-SMELT ANALYSIS ===\n\
        An ingot cracked after exhausting all retry heats. Analyze the failure and fix it.\n\n\
        CRACKED INGOT:\n\
        {ingot_sexp}\n\n\
        BLUEPRINT:\n\
        {blueprint}\n\n\
        CRUCIBLE STATE:\n\
        {crucible}\n\n\
        FAILURE LOGS:\n\
        {failure_logs}\n\n\
        GIT STATE:\n\
        {git_state}\n\n\
        === YOUR TASK ===\n\
        Analyze WHY this ingot failed. Then choose ONE action:\n\n\
        OPTION A - REWRITE: If the work or proof was wrong, output a corrected ingot.\n\
        OPTION B - SPLIT: If the task is too big, split into 2-4 smaller sub-ingots.\n\
        OPTION C - IMPOSSIBLE: If this genuinely cannot be done.\n\n\
        OUTPUT FORMAT (exactly one of):\n\n\
        REWRITE:\n\
        (ingot :id \"{id}\" :status ore :solo t :grade {grade} :skill {skill} :heat 0 :max 5 :smelt 1 :proof \"CORRECTED_PROOF\" :work \"Corrected task description\")\n\n\
        SPLIT:\n\
        (ingot :id \"{id}a\" :status ore :solo t :grade G :skill S :heat 0 :max 5 :smelt 1 :proof \"PROOF\" :work \"Sub-task 1\")\n\
        (ingot :id \"{id}b\" :status ore :solo t :grade G :skill S :heat 0 :max 5 :smelt 1 :proof \"PROOF\" :work \"Sub-task 2\")\n\n\
        IMPOSSIBLE:\n\
        IMPOSSIBLE: reason\n\n\
        RULES:\n\
        - ALL rewritten/split ingots MUST have :smelt 1\n\
        - Fix the ROOT CAUSE, do not just retry the same thing\n\
        - If proof command was wrong, fix the proof\n\
        - If work was too vague, make it specific\n\
        - If task was too large, split into focused sub-tasks\n\
        - Output ONLY the action keyword and ingot lines, nothing else\n",
        ingot_sexp = crate::sexp::writer::write_ingot(ingot),
        id = ingot.id,
        grade = ingot.grade,
        skill = ingot.skill,
    )
}

/// Build the reconsider prompt — surveyor-scoped re-analysis of a twice-failed ingot.
/// Unlike re-smelt (which tweaks proof/work), this questions the fundamental approach.
pub fn prepare_reconsider_flux(ingot: &Ingot, failure_logs: &str) -> String {
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "None".into());
    let crucible = std::fs::read_to_string(CRUCIBLE).unwrap_or_else(|_| "Empty".into());
    let git_state = git_log_and_diff();

    format!(
        "=== RECONSIDER ===\n\
        This ingot has ALREADY been re-smelted once and cracked AGAIN. \
        The previous re-smelt did not fix the root cause. \
        Step back and fundamentally rethink the approach.\n\n\
        TWICE-CRACKED INGOT:\n\
        {ingot_sexp}\n\n\
        BLUEPRINT:\n\
        {blueprint}\n\n\
        CRUCIBLE STATE:\n\
        {crucible}\n\n\
        FAILURE LOGS (both original and re-smelted attempts):\n\
        {failure_logs}\n\n\
        GIT STATE:\n\
        {git_state}\n\n\
        === YOUR TASK ===\n\
        The previous approach failed twice. Do NOT tweak — rethink.\n\
        Ask yourself:\n\
        - Is the task description actually achievable given the codebase state?\n\
        - Is the proof testing the right thing? Is it too brittle?\n\
        - Should this be decomposed completely differently?\n\
        - Are there prerequisites missing from other ingots?\n\n\
        Choose ONE action:\n\n\
        OPTION A - REWRITE: Fundamentally different approach to the same goal.\n\
        OPTION B - SPLIT: Decompose into 2-4 smaller, independently verifiable tasks.\n\
        OPTION C - IMPOSSIBLE: This genuinely cannot be done in the current codebase state.\n\n\
        OUTPUT FORMAT (exactly one of):\n\n\
        REWRITE:\n\
        (ingot :id \"{id}\" :status ore :solo t :grade {grade} :skill {skill} :heat 0 :max 5 :smelt 2 :proof \"NEW_PROOF\" :work \"Fundamentally rethought task\")\n\n\
        SPLIT:\n\
        (ingot :id \"{id}a\" :status ore :solo t :grade G :skill S :heat 0 :max 5 :smelt 2 :proof \"PROOF\" :work \"Sub-task 1\")\n\
        (ingot :id \"{id}b\" :status ore :solo t :grade G :skill S :heat 0 :max 5 :smelt 2 :proof \"PROOF\" :work \"Sub-task 2\")\n\n\
        IMPOSSIBLE:\n\
        IMPOSSIBLE: reason\n\n\
        RULES:\n\
        - ALL output ingots MUST have :smelt 2\n\
        - Do NOT repeat the same proof command if it failed twice\n\
        - Do NOT repeat the same work description\n\
        - Think about what ACTUALLY exists in the repo right now\n\
        - If splitting, each sub-task must be independently verifiable\n\
        - Output ONLY the action keyword and ingot lines, nothing else\n",
        ingot_sexp = crate::sexp::writer::write_ingot(ingot),
        id = ingot.id,
        grade = ingot.grade,
        skill = ingot.skill,
    )
}

/// Build the surveyor prompt
pub fn surveyor_prompt(ore: &str) -> String {
    format!(
        "ROLE: Master Surveyor. Analyze this commission as domain expert.\n\n\
        COMMISSION:\n{ore}\n\n\
        Create a thorough BLUEPRINT:\n\n\
        ## 1. OVERVIEW\nWhat are we building? 2-3 sentence summary.\n\n\
        ## 2. COMPONENTS\nList each major piece:\n- Name\n- Purpose\n- Complexity (1-5)\n- Dependencies\n- Skill: web|api|cli|default\n\n\
        ## 3. ARCHITECTURE\n```\ndir/\n├── file structure\n└── layout\n```\nKey interfaces and data flow.\n\n\
        ## 4. DEPENDENCY GRAPH\n```\n[A] ──▶ [B] ──▶ [C]\n         │\n         └────▶ [D]\n```\n\n\
        ## 5. RISKS\n- High complexity areas\n- Integration points\n- External dependencies\n\n\
        ## 6. FORGING SEQUENCE\n1. Foundation (parallel, :solo t)\n2. Core logic\n3. Integration\n4. Polish/deploy\n\n\
        ## 7. ACCEPTANCE CRITERIA\n- Specific tests\n- Features to verify\n- Quality checks\n\n\
        RULES:\n\
        - You are the EXPERT. Make ALL decisions yourself.\n\
        - NO QUESTIONS. If uncertain, choose the best option.\n\
        - NO PREAMBLE. Output ONLY the blueprint markdown."
    )
}

/// Build the founder prompt
pub fn founder_prompt(ore: &str, blueprint: &str) -> String {
    format!(
        "ROLE: Master Founder. Cast ingots from blueprint.\n\n\
        COMMISSION:\n{ore}\n\n\
        BLUEPRINT:\n{blueprint}\n\n\
        OUTPUT: S-expressions only. One per line. No prose.\n\n\
        TEMPLATE:\n\
        (ingot :id \"i1\" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :proof \"SHELL\" :work \"Task\")\n\n\
        FIELDS:\n\
        - :id = unique (i1, i2, ...)\n\
        - :status = ore (always)\n\
        - :solo = t (parallel ok, no deps) | nil (sequential, has deps)\n\
        - :grade = 1-5 complexity (3+ gets plan mode)\n\
        - :skill = web|api|cli|default (selects tools/plugins)\n\
        - :heat = 0\n\
        - :max = attempts (5 simple, 8+ complex)\n\
        - :smelt = 0 (re-smelt count; system manages this)\n\
        - :proof = shell verification command\n\n\
        PROOF COMMANDS:\n\
        - test -f FILE / test -d DIR\n\
        - grep -q PATTERN FILE\n\
        - node --check FILE\n\
        - npm test / npx playwright test\n\
        - curl -s URL | grep -q PATTERN\n\n\
        RULES:\n\
        - Follow blueprint dependency graph\n\
        - :solo t for independent tasks (can parallel)\n\
        - :solo nil for dependent tasks (sequential)\n\
        - Prefer grade 1-2, split complex work\n\
        - Match :skill to task type\n\
        - Every :proof must be executable shell\n\n\
        OUTPUT ONLY S-EXPRESSIONS:"
    )
}

/// Build the master review prompt for the review phase
pub fn prepare_review_flux(
    ingot_id: &str,
    branch: &str,
    diff: &str,
    ci_result: &crate::pipeline::review::CiResult,
) -> String {
    let fmt_status = if ci_result.fmt_passed {
        "PASSED"
    } else {
        "FAILED"
    };
    let clippy_status = if ci_result.clippy_passed {
        "PASSED"
    } else {
        "FAILED"
    };
    let test_status = if ci_result.test_passed {
        "PASSED"
    } else {
        "FAILED"
    };

    format!(
        "=== MASTER CODE REVIEW ===\n\
        You are the master code reviewer for the slag forge system.\n\
        Review this branch before it can be merged to main.\n\n\
        INGOT: {ingot_id}\n\
        BRANCH: {branch}\n\n\
        === CI RESULTS ===\n\
        - Format check (cargo fmt --check): {fmt_status}\n\
        - Clippy (cargo clippy -- -D warnings): {clippy_status}\n\
        - Tests (cargo test --all): {test_status}\n\n\
        {ci_details}\n\
        === DIFF ===\n\
        {diff}\n\n\
        === YOUR TASK ===\n\
        Review the code changes and evaluate:\n\
        1. Code correctness - does the implementation match the intent?\n\
        2. Code quality - is it clean, idiomatic, maintainable?\n\
        3. Integration safety - will this merge cleanly with main?\n\
        4. Potential issues - bugs, edge cases, security concerns?\n\n\
        OUTPUT FORMAT (exactly this):\n\
        STATUS: APPROVED|REJECTED\n\
        COMMENTS:\n\
        <your detailed review comments, 1-3 sentences>\n\n\
        RULES:\n\
        - If CI passed and code looks reasonable, APPROVE\n\
        - Only REJECT for serious issues\n\
        - Be concise in comments\n\
        - Focus on what matters\n",
        ci_details = if !ci_result.passed() {
            format!(
                "CI FAILURE DETAILS:\n{}\n{}\n{}",
                if !ci_result.fmt_passed {
                    format!(
                        "- fmt: {}",
                        &ci_result.fmt_output[..ci_result.fmt_output.len().min(200)]
                    )
                } else {
                    String::new()
                },
                if !ci_result.clippy_passed {
                    format!(
                        "- clippy: {}",
                        &ci_result.clippy_output[..ci_result.clippy_output.len().min(500)]
                    )
                } else {
                    String::new()
                },
                if !ci_result.test_passed {
                    format!(
                        "- test: {}",
                        &ci_result.test_output[..ci_result.test_output.len().min(500)]
                    )
                } else {
                    String::new()
                }
            )
        } else {
            String::new()
        }
    )
}

fn read_tail(path: &str, lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let all_lines: Vec<&str> = content.lines().collect();
            let start = all_lines.len().saturating_sub(lines);
            all_lines[start..].join("\n")
        }
        Err(_) => "Fresh".into(),
    }
}

fn git_diff_stat() -> String {
    std::process::Command::new("git")
        .args(["diff", "--stat", "HEAD~3"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                let lines: Vec<&str> = s.lines().collect();
                let start = lines.len().saturating_sub(20);
                Some(lines[start..].join("\n"))
            } else {
                None
            }
        })
        .unwrap_or_else(|| "No history".into())
}

fn git_log_and_diff() -> String {
    let diff = git_diff_stat();
    let log = std::process::Command::new("git")
        .args(["log", "--oneline", "-5"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "No commits".into());
    format!("{diff}\n{log}")
}
