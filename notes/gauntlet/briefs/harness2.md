Complexity: frontier — port a research method (recursive agent harnessing) into a production Rust orchestrator, with cost and safety constraints.

# Lane: Harness² in slag

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-harness2 (git worktree, branch gauntlet/harness2). Work only there and in /Users/stas/.claude/jobs/f353e9d3/tmp. Never write anywhere else, in particular never in /Users/stas/Playground/slag itself. Do not call EnterWorktree; use absolute paths.

## Read first

1. The paper: /Users/stas/.claude/jobs/f353e9d3/tmp/paper/harness2.pdf (text: harness2.txt beside it; the stage prompts are in appendix H, from line 1907 of the text). "Harness²: Recursive Agent Harnessing for an Open World" (Google, 2026). Read sections 2, D and H fully.
2. slag: `CLAUDE.md` and `README.md` in your worktree, then `slag-rs/src/pipeline/fix.rs` (+ `fix/`), `pipeline/duel.rs` (casts in isolated worktrees + a judge), `pipeline/warden.rs` (fresh, read-only judge), `engine/recipes.rs` (skills: `recipes/<name>/RECIPE.md`), `engine/prompt.rs` and `flux.rs` (prompt assembly), `engine/hooks.rs` (lifecycle hooks), `engine/sandbox.rs` (shell sandbox, read-only mode).

## The method in one paragraph

A frozen model plus an editable harness (system prompt, guardrails, memory, tool guidance, skills, scripts, subagents, middleware plugin), each a file. Task-level recursion: run the task with the current harness as a reference; propose two candidate harness edits, one INVARIANT (agentic habits that recur across domains: planning, verification, tool use) and one DOMAIN (knowledge and procedures for this kind of task); PROBE each candidate by executing the same task under matched conditions; COMPOSE a successor harness from the edits the behavioral contrast supports (merge, revise, adopt one, or keep the current), execute it, and record why. k controls parallel width or sequential depth; a blind selector picks the final deliverable when k > 1. Harnessing-level recursion: after the task, a reflector reads the whole improvement record and updates a LIBRARY (adopted harness tree + EXPERIENCE.md + an index); later tasks start from the base harness but Propose/Compose can read and reuse library entries. No ground-truth labels: probes give behavioral evidence, not scores.

## Goal

slag gains recursive harnessing, so that over a stream of issues `slag fix` gets measurably better at the kind of work a repository brings, without model changes, while the default fast path stays fast and unchanged.

## Requirements (you decide the design within them)

1. A file-based execution harness that slag's prompts and tools actually load: map the paper's components onto slag's real surfaces (prompt bands, recipes as skills, scripts run through the sandboxed shell, hooks as the middleware equivalent; say which paper component has no slag equivalent and why). An empty harness must reproduce today's behavior exactly.
2. Task-level recursion for `slag fix` behind an explicit option (for example `slag fix <issue> --evolve 1`): reference execution, invariant + domain proposals by an editor role (configurable model, default the plan model), probes executed in isolated worktrees (reuse the duel/anvil machinery where it fits), composition, composed execution, and for k > 1 a blind selector. In `slag fix` the repo's tests exist: use their exit codes as part of the behavioral evidence, never as a reason to skip the contrast, and never let a harness edit touch the repo's tests or code under fix (the harness is slag's, the repo is the user's).
3. Harnessing-level recursion: a reflector after each evolved task updates a library (per user, keyed by domain, with an index; per-repo override allowed). A cheap `--harness library` path (or similar) applies relevant library entries with no probing, so users get the transfer benefit at normal speed. Lessons enter the library only when backed by an observed contrast.
4. Safety: editor, reflector and selector sessions get evidence as files in a working dir and write only harness files there; every model-run command stays in slag's sandbox; harness scripts run sandboxed like any model command; an invalid harness (missing files, a script that does not run, a skill with no trigger) is retried once with the validator's message, then falls back to the tree it started from, and the fallback is recorded. No grader data, hidden tests or bench paths ever enter an evidence dir.
5. Cost and visibility: per-stage tokens, time and spend on the done screen and in `slag status --json`; hard budget caps; a run that hits a cap says so and keeps the best verified deliverable.
6. Tests with the mock provider for each stage, the fallback path, the library update, and "empty harness = identical prompts".
7. A real smoke on 2-3 tasks of your own (outside the repo) with a cheap provider (`--provider mistral` or `openai`; Cerebras has no credit; keys in env, never print them; spend ≤ $3): show one task where the composed harness changed behavior and why, and the library entry it produced.
8. Docs: a "Recursive harnessing" section in README.md and the config table in CLAUDE.md; cite the paper.

A separate benchmark (an ordered stream of harder issues with hidden tests) and a separate critic will judge this later. You will not see the hidden tests; do not read `bench/streams/**/hidden`.

## Hard rules

- `cd slag-rs && CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test` passes by cargo's own exit code; `cargo clippy --all-targets -- -D warnings` stays clean (the branch is at zero warnings).
- Minimal changes outside new modules; match code style and comment density. Prose: no em dashes, active voice, short sentences.
- Read `flux.rs` `DOCUMENT_OUTPUT_RULE` before writing any prompt whose output is a document.
- Commit on `gauntlet/harness2` in focused commits. Do not push. No version bump.

## Report (≤400 words)

The component mapping table; the CLI surface; how a contrast is formed and decided; the library layout; the smoke results (behavior change, library entry, tokens/time/spend per stage); known gaps.
