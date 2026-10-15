# slag issue-solving bench

Measures any coding agent on "here is a repo and a bug report, fix
it". For each task the runner hands the agent a fresh copy of a small
buggy repo plus an `ISSUE.md`, then scores the result with held-out
tests the agent never saw. Output: pass/fail per task, wall-clock time,
and a JSON record.

## Layout

- `issues/<task-id>/repo/` - the buggy project with a visible test
  suite that passes as-is (or covers unrelated behavior).
- `issues/<task-id>/ISSUE.md` - the bug report, in a user's voice.
- `issues/<task-id>/hidden/` - held-out tests. Fail on the buggy repo,
  pass on any correct fix.
- `issues/<task-id>/check.sh` - runs the hidden tests against a repo
  dir: `check.sh <repo_dir>`, exit 0 means solved.
- `issues/<task-id>/reference.patch` - one correct fix, git-apply
  format relative to `repo/`.
- `issues/<task-id>/meta.json` - id, lang, difficulty, summary.
- `agents/` - adapters that run an agent on a task.
- `results/<timestamp>/` - per-run logs, work dirs, and JSON.

Tasks: 3 Python (stdlib unittest), 3 JavaScript (node:test), 1 Rust
(cargo test, no external crates), 1 shell. No network, no package
installs.

## Running

    bench/run.sh --agent bench/agents/codex.sh
    bench/run.sh --agent bench/agents/codex.sh --tasks py-passcheck,js-leaderboard --runs 3 --timeout 900
    bench/run.sh --agent bench/agents/fx.sh --out results/fx.json

Options: `--tasks` (comma list), `--timeout` (seconds, default 600),
`--runs` (repetitions, default 1), `--out` (JSON path, default
`results/<timestamp>/results.json`).

Per task and run the runner copies `repo/` to a fresh work dir under
`results/<timestamp>/work/`, commits a base, runs the adapter, then
copies `hidden/` in and runs `check.sh`. It records pass/fail, wall
seconds, exit code, timeouts, `git diff --shortstat` against the base,
and the adapter output log.

## Adapter contract

An adapter is an executable invoked as:

    <adapter> <issue_file> <repo_dir>

It gets `BENCH_TASK_DIR` in the environment (the task directory, where
`reference.patch` lives). Anything the adapter prints goes to the run
log. Bundled adapters: `noop.sh` (does nothing), `reference.sh`
(applies the reference patch), `codex.sh`, `fx.sh`, `slag-fix.sh`
(placeholder for the future `slag fix` command).

## Instrument checks

    bench/run.sh --verify-fixtures

Every task's hidden suite must fail on the untouched repo and pass
after `reference.patch` is applied. Additionally, the noop adapter must
score 0/8 and the reference adapter 8/8:

    bench/run.sh --agent bench/agents/noop.sh
    bench/run.sh --agent bench/agents/reference.sh

## Adding a task

1. Create `issues/<id>/repo/` with one real bug and a passing visible
   test suite. No network, no package installs. Python: stdlib
   unittest. JavaScript: `node --test` only. Rust: no external crates,
   must build offline.
2. Write `ISSUE.md` as a user: symptom, reproduction, expected vs
   actual. Never name the fix or point at the buggy file unless a real
   user could.
3. Write `hidden/` tests that assert behavior, not implementation.
   They must fail on the buggy repo and pass for any correct fix.
4. Write `check.sh` (runs hidden tests against `$1`), `meta.json`, and
   regenerate the patch: `bench/regen-patches.sh` (edit it to add your
   fix).
5. Run `bench/run.sh --verify-fixtures --tasks <id>`.
