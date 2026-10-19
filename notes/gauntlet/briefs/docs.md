Complexity: standard — documentation truth pass across README, CLAUDE.md and the slag.dev page, from an existing audit; plus a one-line code message fix.

# Lane: docs and site tell the truth

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-docs (git worktree, branch gauntlet/docs). Write only there and in /Users/stas/.claude/jobs/f353e9d3/tmp. Do not call EnterWorktree. NEVER run a bare `slag` (the installed v2.9.0 on PATH is stale and turns `slag fix --help` into a paid commission); build and call `slag-rs/target/debug/slag` by absolute path, with `SLAG_CONFIG_DIR` set to an empty scratch dir and no API keys in the environment (`env -i HOME=$HOME PATH=/usr/bin:/bin ...`) when you run `--help`.

## Ground truth
The built binary's help output (`slag --help`, and `--help` of every subcommand: fix, key, status, runs, insights, ps, resume, update, rewind, cost, hooks, replay) and the source in `slag-rs/src` (config.rs for settings and env vars; engine/provider.rs presets incl. the OpenAI Responses API for gpt-5.4+; engine/sandbox.rs; harness/; pipeline/fix*; pipeline/direct*; pipeline/warden*; sexp/parser.rs has 13 fields incl. `:bar` and `:casts`).

## Audit to act on (verified by the lead)
Read /Users/stas/Playground/slag/.claude/worktrees/gauntlet-production/notes/gauntlet/briefs/docs-audit.md (the full audit). Highlights: "eleven fields" is wrong (13) in README, CLAUDE.md and the site; "4-phase"/"Four Phases" omits the temper (warden) stage; chat-completions-only claims contradict the Responses API support; new commissions keep coordination files under `.slag/` (root PRD.md/PLAN.md only for existing projects); missing subcommands (replay, runs, insights, ps, rewind, hooks) and flags (--temper, --force, --trace, --duel, --no-sandbox; fix: --tries, --test, --temper, --tui, --model, --plan-model, --evolve, --harness, ...); missing env vars (SLAG_SANDBOX, SLAG_MODEL_EDITOR, SLAG_<PROVIDER>_BASE_URL, SLAG_HARNESS_LIBRARY, SLAG_EVOLVE_BUDGET, SLAG_EVOLVE_TOKENS, SLAG_MODEL_FALLBACK, SLAG_UNATTENDED_RETRY, SLAG_CREDIT_FLOOR, SLAG_MAX_RETRIES, SLAG_API_TIMEOUT_MS, SLAG_BASH_OUTPUT_CAP, SLAG_COST_PER_MTOK, SLAG_ALLOW_DESTRUCTIVE, SLAG_DISABLE_HOOKS); the site never mentions the shell sandbox, spend estimates, `slag replay`, or `.slag/`.
Release truth: the published v2.9.0 ships macOS Apple Silicon only; `slag-rs/dist-local/publish-release.sh --build` now builds four targets (Apple Silicon, Intel Mac, Linux x86_64 and aarch64) and `install.sh`/`slag update` support all four. Say "the next release ships four targets" only where a reader needs it; do not claim a published Linux build exists.
Code fix (one line plus a comment): hooks are read from `config.toml` (`[hooks]` table, see config.rs:1133), but `engine/hooks.rs:5` and the user-facing `slag hooks list` hint at `engine/hooks.rs:967` say `slag.toml`. Correct both. `cargo test` must stay green (cargo's own exit code) and `cargo clippy --all-targets -- -D warnings` clean.

## Harness² wording
`slag fix --evolve K` and `--harness library` implement "Harness²: Recursive Agent Harnessing" (arXiv 2610.14224). Describe what they do and what they cost, mark them experimental, and do not claim an accuracy gain; leave one sentence in README marked `<!-- harness2-result -->` that the lead will fill with the measured result.

## Deliverables
1. README.md: correct every false/stale item; add the missing commands, flags, env vars, files (`.slag/` layout), the sandbox section stays accurate; keep the structure; tighten, do not bloat.
2. CLAUDE.md: the same truth fixes for the agent-facing guide (source layout incl. render/, replay.rs, insights.rs, harness/, pipeline/direct, warden, hygiene; config tables; binary surface).
3. website/index.html (+ src/*.js only if needed): fix the false lines (13 fields; phases incl. temper; `.slag/` paths; CLI reference and env table additions; a short, truthful sandbox and spend line in Features). Keep the design and the hero video untouched; keep copy calm (no hype words). `cd website && npm run build` passes.
4. bench/README.md: one line noting the adapter's default empty `SLAG_CONFIG_DIR`.
Prose rules everywhere: no em dashes, active voice, short sentences, concrete.
Commit on `gauntlet/docs` (`docs: ...`, `website: ...`, `hooks: ...`). Do not push. Report ≤200 words: what changed per file, and anything you could not verify.
