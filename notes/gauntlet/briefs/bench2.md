Complexity: standard — extend an existing, validated benchmark harness with a harder, ordered task stream and measure a weak baseline.

# Lane: a harder issue stream with headroom (the instrument for Harness²)

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-bench2 (git worktree, branch gauntlet/bench2). Work only here and in /Users/stas/.claude/jobs/f353e9d3/tmp. Never write anywhere else, in particular never in /Users/stas/Playground/slag itself.

## Why

slag (Rust coding-agent CLI in `slag-rs/`) has `slag fix <issue>`. The existing bench in `bench/` (read `bench/README.md` and `bench/run.sh` first; you built it) has 8 tasks and slag now solves 16/16 on a fast model, so it cannot show improvement. slag is about to get "recursive harnessing" (the Harness² paper: an agent improves its own prompts, guardrails, skills, scripts and memory from task to task, keeping a library of what worked). That needs a stream of related tasks where a weak, cheap executor model fails often enough to leave room, and where lessons from earlier tasks can help later ones.

## Deliverables

1. `bench/streams/<domain>/` for 2 domains, each an ORDERED stream of 8 tasks (16 total): `py-data` (Python data-wrangling library bugs: parsing, dates, unicode, rounding, CSV/JSON edge cases) and `js-web` (Node utilities: URL/query handling, async ordering, pagination, caching, error handling). Same per-task layout as `bench/issues/` (repo/, ISSUE.md, hidden/, check.sh, reference.patch, meta.json with an `order` field). Make tasks within a domain share recurring traps a good harness could learn once (for example: always reproduce with the exact input from the issue; check timezone-naive datetimes; check unicode code points not UTF-16 units; run the full suite, not one file; float rounding with Decimal), so a lesson from task 2 really helps task 6. Mix difficulty: 2 easy, 4 medium, 2 hard per domain. No network, no package installs, stdlib unittest and node:test only.
2. `bench/run.sh` gains `--stream <domain>` (run that domain's tasks in `order`, one run each, carrying nothing between tasks by itself; the agent under test decides what to carry) and keeps `--verify-fixtures`, noop and reference checks working for streams. Keep the leak protections (issue copied to an empty temp dir; no task paths in env).
3. Instrument checks, run and report: `--verify-fixtures` passes for all 16; noop scores 0/16; reference 16/16; the leak probe passes.
4. Find a weak, cheap executor with headroom: run `SLAG_BIN=<path> SLAG_PROVIDER=<p> SLAG_MODEL_BASE=<m> bench/run.sh --agent bench/agents/slag-fix.sh --stream <domain>` for a couple of cheap models (try `mistral` presets such as `ministral-8b-latest` or `mistral-small-latest`, and `openai::gpt-4.1-nano`; Cerebras has no credit; keys are in env, never print them; spend ≤ $2) and pick one that passes 25-60% of each stream. Build slag first: `cd slag-rs && CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo build`. Save the baseline results JSON per domain to `bench/results/stream-baseline-<domain>.json` and report the table.
5. Update `bench/README.md` with the stream mode, the chosen weak executor, and the baseline.

## Hard rules

Edit only `bench/**`. Prose: no em dashes, active voice, short sentences. Commit on `gauntlet/bench2` (`bench: ...`). Do not push. When done, list files changed, the instrument check numbers, and the baseline table.
