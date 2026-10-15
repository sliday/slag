Complexity: hard — cross-module state-machine bug in the forge loop (crucible status, re-smelt, duel scheduling); must not regress ~850 tests.

# Lane: a cracked ingot must stay cracked

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-loop (git worktree, branch gauntlet/loop). Work only here.

## Context

slag is a Rust coding-agent orchestrator in `slag-rs/`. Read `CLAUDE.md` at the repo root first (ingot fields `:status`, `:heat`, `:max`, `:smelt`, `:duel`; FORGE; re-smelt; duel). Then read `slag-rs/src/pipeline/forge.rs`, `pipeline/resmelt.rs`, `pipeline/duel.rs`, `crucible.rs`.

## The defect (seen by an independent critic, real run)

A single ingot exhausted its heats ("⚒ 5/5 ✗ no CMD"), the forge printed "⚠ already re-smelted, truly cracked", the status line then showed `[ ✅0 done | 🔥0 forging | 🧱1 queued | ❌1 failed ]`, and the forge started a new duel ("⚔ duel: 2 casts ... — 1 round") on the same ingot. It repeated 135 rounds in 12 minutes, paying for model calls each time, until the critic killed it. Evidence: `/Users/stas/.claude/jobs/f353e9d3/tmp/critic-provider/run-mixed/out.log` (read from line 250 on) and that run's `logs/` and `PLAN.md`. The trigger was a model that never produced a verification command, but any ingot that cracks after a re-smelt can hit it.

## Goal

A cracked ingot stays cracked, a run always terminates, and money stops being spent once nothing can make progress.

## Deliverables

1. Find the root cause (why a truly cracked ingot, or a phantom "queued" one, goes back to an anvil). Explain it in two or three sentences in your final message, with file:line.
2. Fix it at the root. Add a structural guard as well: the forge loop must terminate when a full scheduling pass makes no state change (no ingot changed status, heat, or smelt), and must never schedule an ingot whose status is cracked or forged.
3. A regression test that reproduces the 135-round loop with the existing mock provider (look for `MockProvider` and the `agent()` test helper in `engine/agent.rs` tests, and how forge tests drive the pipeline) and proves it now stops. No live API calls.
4. `cd slag-rs && CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test` passes.

## Paths you own (edit nothing else)

`slag-rs/src/pipeline/forge.rs`, `slag-rs/src/pipeline/resmelt.rs`, `slag-rs/src/pipeline/duel.rs`, `slag-rs/src/crucible.rs`, tests in those files or new files under `slag-rs/tests/`. Do NOT touch `pipeline/mod.rs`, `pipeline/fix*`, `engine/`, `config.rs`, `tui.rs`, `dashboard.rs`, `render/`, `website/`.

## Hard rules

- Minimal diff. Match the code style and comment density.
- Prose (comments, commit messages): no em dashes, active voice, short sentences.
- Commit on branch `gauntlet/loop` as `forge: one sentence`. Do not push. No version bump.

When done, list the root cause, files changed, and the exact commands that verify the work.
