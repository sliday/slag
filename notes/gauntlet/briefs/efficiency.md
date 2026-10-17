Complexity: hard — cross-module cost reduction in the commission pipeline (forge bookkeeping, warden, agent context), verified by a real rerun.

# Lane: a small commission must cost what it is worth

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-efficiency (git worktree, branch gauntlet/efficiency). Work only here.

## Context

slag is a Rust coding-agent orchestrator in `slag-rs/`. Read `CLAUDE.md` first. A real run of `slag "write wc.py that counts words in a file, with a unittest" --temper 1 --provider cerebras` took 7:56 and 2,276,015 tokens (≈ $2.44) for an 800-byte script and a 2.5 KB test. A read-only analysis of that run's logs is in `notes/gauntlet/cost-analysis-wc-commission.md` (in this worktree; read it fully, it has the per-session table and file:line refs). The run's artifacts: /Users/stas/.claude/jobs/f353e9d3/tmp/critic-tui-r2/commission/.

The three drivers it found:
1. Coordinator-state rollback, 837k tokens (37%): the smith restored PLAN.md/PROGRESS.md to satisfy cleanliness checks, restoring stale ingot states, so the coordinator repeated i1, i2, then i1; the warden rejected correct code because slag's own coordinator files were dirty (`pipeline/forge.rs:1182` commit before ledger, `:287` save status after, `:133` reload, `proof.rs:64` `git add -A`).
2. Conversation growth: every request re-sends the full conversation (`engine/agent.rs:877`); the 150k default budget (`engine/agent.rs:29`) never triggers; forge prompts are 28-33k characters.
3. Invented acceptance constraints, ≥ 289k tokens: the bar prompt asks for harsh mechanical rules even an unreachable bar (`flux.rs:378`); the warden banned a normal local `import wc`, forcing another founder pass; a generated test-count regex could not match the mandated method names; wardens run with full forge tools (`smith/mod.rs:75`, `pipeline/warden.rs:234`), eleven warden sessions cost 649k tokens.

## Goal

The same commission, on the same provider and model, finishes with a correct artifact and a real warden verdict for a small fraction of the cost and time, and no step of slag's own bookkeeping can fail or loop the run.

## Acceptance (measured by a real rerun; report the numbers)

- Rerun the exact commission above in a fresh scratch dir with `--provider cerebras` (key in env, never print it): FORGED, the warden reaches a verdict, `python3 -m unittest` passes in the result, and total tokens ≤ 200,000 and wall time ≤ 120 s. Stretch target from the analysis: 20-40k tokens, 15-30 s. Report the per-phase token table for your run in the same shape as the analysis.
- Run a second, slightly larger commission of your choice (3-4 files) to show you did not tune to one example; report tokens and time.
- `cd slag-rs && CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test` passes by cargo's own exit code.
- Live spend ≤ $1.50 total.

## Direction (you decide the design)

- slag's coordinator files (PLAN.md, PROGRESS.md, BLUEPRINT.md, BAR.md, PRD.md, AGENTS.md, logs/) are slag's, not the model's: model commands must not restore or rewrite them, and no cleanliness check or bar may count them as dirt. Order slag's own commits so the tree is clean after its bookkeeping.
- Bars and wardens judge the commission's real requirements, not invented rules. Keep the warden independent and honest, but give it a bounded, cheaper way to inspect (fewer tools or a turn budget) if that keeps verdict quality.
- Context: keep what the model needs (task, recent tool rounds, a compact evidence summary) instead of replaying everything; measure the effect.
- If a small commission does not need the blueprint and plan negotiation rounds, find a principled way to skip or shorten them.

## Paths you own

`slag-rs/src/pipeline/forge.rs` (bookkeeping, scheduling and commit ordering; NOT the git command construction lines: another builder is wrapping every git call slag makes in a hardened helper, so call the existing functions and do not rewrite how git is invoked), `slag-rs/src/pipeline/warden.rs`, `slag-rs/src/pipeline/surveyor.rs`, `slag-rs/src/pipeline/founder.rs`, `slag-rs/src/pipeline/mod.rs`, `slag-rs/src/flux.rs`, `slag-rs/src/engine/agent.rs`, `slag-rs/src/engine/compact.rs`, `slag-rs/src/smith/**`, `slag-rs/src/engine/prompt.rs`, tests. Do NOT touch `proof.rs`, `engine/sandbox.rs`, `engine/tools.rs`, `pipeline/fix*`, `tui.rs`, `dashboard.rs`, `render/`, `website/`, `bench/`, `engine/provider.rs`, `engine/pricing.rs`.

## Hard rules

- Read `flux.rs` `DOCUMENT_OUTPUT_RULE` and the finish-summary trap before changing any prompt whose output is a document.
- Minimal, well-placed diffs; match code style and comment density. Prose: no em dashes, active voice, short sentences.
- Commit on `gauntlet/efficiency` (`forge: ...`, `warden: ...`, `agent: ...`). Do not push. No version bump.

When done, list the files changed, both measured runs (tokens, seconds, per-phase table), what you cut and why, and the exact verify commands.
