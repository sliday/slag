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
            "!!! CRACKED - PREVIOUS ATTEMPT FAILED !!!\n{slag_msg}\n!!! ANALYZE AND FIX !!!\n\n\
            End with exactly: CMD: <shell command to verify>\n"
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

/// Human line for whole-job spend, restored from
/// `.slag/session-costs.json`. A re-melt is the same job resumed, so the
/// figure covers every prior invocation, not just the last one.
pub fn spend_note(record: Option<&crate::dashboard::CostRecord>) -> String {
    match record {
        Some(r) => {
            let mut s = format!("{} tok", r.total_tokens);
            if let Some(cost) = r.cost {
                s.push_str(&format!(" · ${cost:.2}"));
            }
            s.push_str(" spent across all invocations of this job");
            s
        }
        None => "none recorded".into(),
    }
}

/// Reload the persisted whole-job spend for the current crucible.
fn session_spend() -> String {
    let plan = std::fs::read_to_string(CRUCIBLE).unwrap_or_default();
    let run_id = crate::dashboard::run_id_for_plan(&plan);
    spend_note(crate::dashboard::load_session_cost(&run_id).as_ref())
}

/// Build the re-smelt analysis prompt for a cracked ingot
pub fn prepare_resmelt_flux(ingot: &Ingot, failure_logs: &str) -> String {
    let blueprint = std::fs::read_to_string(BLUEPRINT).unwrap_or_else(|_| "None".into());
    let crucible = std::fs::read_to_string(CRUCIBLE).unwrap_or_else(|_| "Empty".into());
    let git_state = git_log_and_diff();
    let spend = session_spend();

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
        JOB SPEND SO FAR: {spend}\n\n\
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
        - :proof = shell verification command\n\
        - :duel = t (force multi-cast duel) | nil (block it) | omit for auto (grade-gated)\n\
        - :casts = 1|2|3 parallel smiths forging the ingot; omit for auto\n\n\
        CASTS HEURISTICS (set :casts per ingot):\n\
        - 1: grade <= 2, mechanical work (create/rename/config), a deterministic proof \
        (test -f / grep -q), or :solo nil\n\
        - 2: grade 3-4, design-choice work (API shape, refactor strategy, UX), or a retry \
        after a crack\n\
        - 3: grade 5, or taste-dominant polish work\n\n\
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::{Skill, Status};

    fn sample_ingot() -> Ingot {
        Ingot {
            id: "i1".into(),
            status: Status::Ore,
            solo: true,
            grade: 1,
            skill: Skill::Default,
            heat: 1,
            max: 5,
            smelt: 0,
            proof: "true".into(),
            work: "Do the thing".into(),
            duel: None,
            casts: None,
            extra: vec![],
        }
    }

    #[test]
    fn first_heat_flux_states_cmd_contract() {
        let flux = prepare_flux(&sample_ingot(), None);
        assert!(flux.contains("CMD: <shell command to verify>"));
    }

    #[test]
    fn founder_prompt_documents_the_casts_field() {
        let prompt = founder_prompt("build it", "the plan");
        assert!(prompt.contains(":casts = 1|2|3"), "field table must list :casts");
        assert!(prompt.contains("CASTS HEURISTICS"), "heuristics block missing");
        for tier in ["- 1: grade <= 2", "- 2: grade 3-4", "- 3: grade 5"] {
            assert!(prompt.contains(tier), "missing tier {tier:?}");
        }
    }

    /// The re-melt prompt carries whole-job spend restored from
    /// `.slag/session-costs.json` — the smith budgets against the job,
    /// not the invocation.
    #[test]
    fn resmelt_flux_reports_whole_job_spend() {
        let flux = prepare_resmelt_flux(&sample_ingot(), "boom");
        assert!(flux.contains("JOB SPEND SO FAR:"), "spend section missing");
    }

    #[test]
    fn spend_note_formats_restored_records_and_the_fresh_case() {
        let rec = crate::dashboard::CostRecord {
            total_tokens: 118_234,
            cost: Some(0.31),
            ..Default::default()
        };
        assert_eq!(
            spend_note(Some(&rec)),
            "118234 tok · $0.31 spent across all invocations of this job"
        );

        // Costless providers still report tokens.
        let free = crate::dashboard::CostRecord { total_tokens: 42, ..Default::default() };
        assert_eq!(spend_note(Some(&free)), "42 tok spent across all invocations of this job");

        assert_eq!(spend_note(None), "none recorded");
    }

    #[test]
    fn retry_flux_restates_cmd_contract() {
        let flux = prepare_flux(&sample_ingot(), Some("CMD failed (exit 1): boom"));
        assert!(flux.contains("CRACKED"));
        assert!(
            flux.contains("CMD: <shell command to verify>"),
            "retry flux must keep the CMD contract"
        );
    }
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
