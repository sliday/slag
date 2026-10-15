Complexity: hard — new pipeline entry point that must be fast and correct on real repos; design judgment about what to skip.

# Lane: `slag fix` — the fast issue-solving harness

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-fix (git worktree, branch gauntlet/fix). Work only here, with absolute paths.

## Context

slag is a Rust coding-agent orchestrator in `slag-rs/`. Read `CLAUDE.md` at the repo root first. Today's entry point is a commission: `slag "build X"` runs SURVEYOR (BLUEPRINT.md) → FOUNDER (PLAN.md ingots) → FORGE (agent sessions with tools, `:proof` checks, retries, re-smelt) → optional warden (`--temper`, judges the artifact against BAR.md) → ASSAY. That ceremony is right for building a project and far too slow for "here is a bug report, fix it in this repo".

The owner's words: "I need a fast LLM-provider-agnostic harness for solving issues." Another builder is making the engine provider-agnostic underneath you (`config.rs`, `engine/provider.rs`). Build on the existing smith factories (`smith/`) so provider choice falls out for free; do not touch provider code.

## Goal

`slag fix <issue>` solves an issue in the current repository, fast, and proves it.

- Input forms: free text (`slag fix "dates off by one in export"`), a file (`--issue-file path`), a GitHub issue number in the current repo (`#123` or `123`, fetched with `gh issue view`), or a GitHub issue URL.
- It works in an existing repo with its own code and tests. It never treats the repo as a fresh commission and never writes PRD/BLUEPRINT/PLAN files into the user's repo root.
- It ends with a verified fix: it finds the repo's real test command, reproduces the bug (a failing test or command) before fixing when it can, fixes it, and runs the tests again. The verdict it prints is based on commands it ran, not on the model's say-so.
- It leaves the work reviewable: commits on a new branch (`slag/fix-<slug>` or `slag/fix-<n>-<slug>`) when the repo is a git repo, prints the diff stat and the verification command. `--pr` opens a draft PR with `gh` (title from the issue, body with the reproduction, the fix, and the verification output). `--no-commit` leaves changes in the working tree.
- Fast is a feature: no survey/found passes for an issue a single agent session can handle. You decide the routing (for example: one session for normal issues, escalation to the existing ingot machinery only when the issue is too big). Measure wall-clock on your own smoke fixtures and cut waste you find (redundant passes, oversized prompts, serial work that can run in parallel).
- Optional `--temper` reuses the existing warden to judge the fix independently. Keep it off by default unless your measurements say it is cheap.
- Works in stream mode (plain output) and with `--tui`. Use the existing event types so the dashboard shows progress; do not edit `tui.rs` or `dashboard.rs` (another builder owns them; if you need a new event variant, add it in `engine/events.rs` with a minimal change and note it in your final message).

## Measuring

A separate benchmark (seeded-bug repos with hidden tests) will judge you later; you will not see it. Make your own 3 small smoke fixtures in a temp dir OUTSIDE the repo (one Python stdlib-unittest repo, one Node `node:test` repo, one tiny Rust crate), each with a real bug and an issue text, and measure pass/fail and wall time per run. Use fast, cheap models for iteration: the environment has CEREBRAS_API_KEY, GROQ_API_KEY and OPENROUTER_API_KEY (never print them). Until the provider lane lands, `SLAG_OPENROUTER_BASE` plus a matching key can point slag at an OpenAI-compatible endpoint, or just use OpenRouter with a fast model via `--model`. Keep live spend under $3 total.

Also compare once against `codex exec` on the same 3 fixtures (`codex exec -C <repo> -s workspace-write --skip-git-repo-check -m gpt-6.1-sol -c model_reasoning_effort=medium "<issue text> Fix it and run the tests."`) and report both tables.

## Paths you own (edit nothing else)

New `slag-rs/src/pipeline/fix.rs` (and submodules under `slag-rs/src/pipeline/fix/` if you want), `slag-rs/src/pipeline/mod.rs` (registration and shared helpers only), `slag-rs/src/cli.rs` (the `Fix` subcommand only), `slag-rs/src/main.rs` (the dispatch arm only), `slag-rs/src/flux.rs` (new prompt functions only; do not change existing prompts), `slag-rs/src/engine/events.rs` (minimal additions only), new tests under `slag-rs/tests/` and unit tests in your files, and a short `slag fix` section in `README.md`.

Do NOT touch `config.rs`, `engine/provider.rs`, `smith/`, `tui.rs`, `dashboard.rs`, `render/`, `website/`, `bench/`, or the version in `Cargo.toml`.

## Hard rules

- Read `slag-rs/src/flux.rs` for `DOCUMENT_OUTPUT_RULE` and the ≤120-word finish-summary trap before you write any prompt whose output is a document; the finish tool has a dedicated `cmd` field for the verification command (see `engine/tools.rs`).
- `cd slag-rs && cargo test` must pass. Set `CARGO_PROFILE_DEV_DEBUG=line-tables-only` to keep the target dir small.
- Minimal diffs in shared files. Match the surrounding code style, naming, and comment density.
- Prose (docs, comments, commit messages, CLI text): no em dashes, active voice, short sentences.
- Commit on branch `gauntlet/fix` in your worktree with messages like `fix: one sentence` (area prefix `pipeline:` or `cli:` also fine). Do not push. No version bump.
- Never run the fix command against this slag repo itself or any repo outside your temp fixtures.

## Report (≤300 words)

Files changed; the exact CLI surface; the routing you chose and why; your smoke table (fixture, pass/fail, wall seconds) for slag fix and for codex; what you cut to make it fast; known gaps.
