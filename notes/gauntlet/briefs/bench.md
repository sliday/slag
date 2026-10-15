Complexity: standard — a self-contained benchmark harness and eight small fixture repos; known patterns, no novel algorithms, but the instrument must be correct.

# Lane: issue-solving benchmark (the measuring instrument)

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-bench (git worktree, branch gauntlet/bench). Work only here.

## Why this exists

slag (a Rust coding-agent orchestrator in `slag-rs/`) is getting a fast `slag fix <issue>` command. We need an honest, repeatable way to measure any coding agent on "here is a repo and a bug report, fix it": pass rate and wall-clock time. You build that instrument. Another builder writes `slag fix`; that builder will NOT see your hidden tests, so do not leak fixes into anything an agent reads.

## Deliverables

1. `bench/issues/<task-id>/` for 8 tasks. Each task has:
   - `repo/`: a small, realistic project (2 to 8 source files) with one real bug, plus a visible test suite that passes on the buggy code or covers only unrelated behavior.
   - `ISSUE.md`: a bug report in a user's voice (symptom, how they hit it, expected vs actual). No hint at the fix, no file names unless a real user would know them.
   - `hidden/`: held-out tests that fail on the buggy code and pass on a correct fix. Test behavior, not implementation details, so any correct fix passes.
   - `check.sh`: copies nothing; runs the hidden tests inside a given repo dir (`check.sh <repo_dir>`), exit 0 means solved.
   - `reference.patch`: a correct fix (git-apply format, relative to `repo/`).
   - `meta.json`: `{ "id", "lang", "difficulty": "easy|medium|hard", "summary" }`.
2. Task mix: 3 Python (stdlib `unittest` only; pytest is NOT installed), 3 JavaScript (Node 24 built-in `node:test`, no npm deps), 1 Rust (`cargo test`, no external crates, must build offline), 1 shell/CLI (bash script with a test runner in bash). Difficulty: 3 easy (one-line logic slip), 3 medium (edge case across two files, off-by-one in pagination, timezone/date boundary, unicode), 2 hard (misleading symptom: the visible error points at the wrong file; or the fix needs a reproduction first, such as a caching or ordering bug).
3. Runner `bench/run.sh`:
   - `bench/run.sh --agent <adapter> [--tasks id1,id2] [--timeout 600] [--runs 1] [--out results.json]`
   - For each task and run: copy `repo/` to a fresh temp dir, `git init`, commit everything as the base, then run the adapter as `<adapter> <issue_file> <repo_dir>` with the timeout, measuring wall time. After the agent exits, copy `hidden/` into the repo (a path the agent never saw) and run `check.sh`. Record per task: pass/fail, wall seconds, exit code, timed_out, lines changed (`git diff --stat` vs base), and the adapter's stdout/stderr tail to a log file under `bench/results/<timestamp>/`.
   - Print a table at the end: per task and totals (pass rate, median and p90 wall time). Write JSON to `--out`.
   - Works on macOS (bash 3.2 compatible or use `#!/usr/bin/env bash` features carefully; `timeout` may be missing on macOS, so implement the timeout yourself, e.g. with a background watchdog, or use `gtimeout` if present).
4. Adapters in `bench/agents/` (each executable, signature `<issue_file> <repo_dir>`):
   - `noop.sh`: does nothing (instrument check: must score 0/8).
   - `reference.sh`: applies the task's `reference.patch` (it may locate the task from an env var the runner sets, e.g. `BENCH_TASK_DIR`; instrument check: must score 8/8).
   - `codex.sh`: `codex exec -C <repo_dir> -s workspace-write --skip-git-repo-check -m "${CODEX_MODEL:-gpt-6.1-sol}" -c model_reasoning_effort="${CODEX_EFFORT:-medium}" "<prompt built from ISSUE.md>"`. The prompt: fix the issue in this repo, run the existing tests, do not ask questions.
   - `fx.sh`: `cd <repo_dir> && fx ask --no-save --no-color "<same prompt>"`.
   - `slag-fix.sh`: placeholder that calls `slag fix --issue-file <issue_file>` with `cd <repo_dir>`; the command does not exist yet, so leave a clear TODO comment that the interface may change. Do not try to make it pass.
5. Instrument validation (run it, report the numbers): `bench/run.sh --agent bench/agents/noop.sh` scores 0/8 and `--agent bench/agents/reference.sh` scores 8/8. Also confirm every `hidden/` suite fails on the untouched repo (a "baseline must fail" check built into the runner as `--verify-fixtures`).
6. One real measurement: run `bench/run.sh --agent bench/agents/codex.sh --runs 1` over all 8 tasks and save the results JSON to `bench/results/baseline-codex.json`. If codex errors out for reasons outside the bench (auth, quota), stop after 3 attempts and report it.
7. `bench/README.md`: what the bench measures, how to run it, how to add a task, the adapter contract, and the instrument checks. Short.

## Paths you own (edit nothing else)

`bench/**` only. Do not touch `slag-rs/`, `website/`, or root docs.

## Hard rules

- No network dependencies in fixtures. No package installs.
- Hidden tests must never be inside `repo/`. `ISSUE.md` must never mention the fix.
- Prose you write: no em dashes, active voice, short sentences.
- Commit on branch `gauntlet/bench` in this worktree with messages in the style `bench: one sentence`. Do not push.

When done, list the files you changed, the exact commands that verify the work, and the noop/reference/codex results tables.
