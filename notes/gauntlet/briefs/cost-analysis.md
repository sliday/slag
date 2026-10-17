Complexity: hard — cross-module cost analysis of a real run from its logs; read-only.

# Where did 2.3M tokens go?

Read-only analysis. Do not edit any file. Write your report to /Users/stas/.claude/jobs/f353e9d3/tmp/cost-analysis/REPORT.md (create the dir) and print it at the end.

## Context

slag is a Rust coding-agent orchestrator: /Users/stas/Playground/slag/.claude/worktrees/gauntlet-production (read `CLAUDE.md` first; code in `slag-rs/src/`). A commission runs SURVEYOR → FOUNDER → FORGE (agent sessions with tools per ingot, retries "heats", re-smelt, duel casts) → warden checks when `--temper N` → ASSAY.

A real run of `slag "write wc.py that counts words in a file, with a unittest" --temper 1 --provider cerebras` (model qwen-3.8-27b) took 7 min 56 s and about 2.3M tokens, about $2.44 at Cerebras list price, for an 800-byte script and a 2.5 KB test. One of its 4 retries was self-inflicted: ingot i1 heat 3 was judged a fail because slag's own commit for i2 left PLAN.md/PROGRESS.md dirty, against a bar that demanded a clean `git status`.

Run artifacts: /Users/stas/.claude/jobs/f353e9d3/tmp/critic-tui-r2/commission/ (PRD.md, BAR.md, BLUEPRINT.md, PLAN.md, PROGRESS.md, logs/ with per-phase PROMPT/RAW logs, FLUX/STRIKE/ASSAY logs, engine-*.jsonl event logs with token usage, transcripts, and `logs/tape-*.jsonl`). The full terminal output: /Users/stas/.claude/jobs/f353e9d3/tmp/critic-tui-r2/comm.tty.log.

## Deliverable (REPORT.md, ≤ 600 words plus one table)

1. A table: tokens (prompt / completion) and wall time per phase and per role (surveyor, founder, warden on blueprint, warden on plan, each ingot heat, duel casts and judge, re-smelt, warden on the artifact), from the event logs' usage records. Totals must add up to the run total; say if they do not.
2. The top 3 cost drivers with root causes and file:line in `slag-rs/src` (for example: whole conversation re-sent every turn with no prompt caching, oversized system prompt or recipe index per turn, tool outputs not truncated, warden exploring with tools, duel casts on a grade-1 task, re-reading logs, the self-inflicted dirty-tree retry).
3. For each driver, the smallest change that removes most of it and an estimate of the saving in tokens and seconds for this run.
4. A target: what a well-built harness should spend on this commission (tokens, seconds), with reasoning.

Be concrete. Numbers from the logs, not guesses; label any estimate.
