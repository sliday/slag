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

## Streams

`streams/<domain>/` holds an ordered stream of related tasks, built to
measure recursive harnessing: an agent that keeps a library of lessons
should do better on later tasks than an agent that starts cold each
time. A stream task has the same layout as `issues/` tasks, plus an
`order` field (integer) and a `traps` list in `meta.json`.

Two domains, 8 tasks each, ordered easy to hard (2 easy, 4 medium, 2
hard per domain):

- `py-data` - Python data-wrangling library bugs. Recurring traps:
  reproduce with the exact input quoted in the issue, timezone-naive vs
  aware datetimes, unicode normalization, Decimal ROUND_HALF_UP money
  math, CSV edge cases (BOM, quoted newlines, chunk boundaries).
- `js-web` - Node utility bugs. Recurring traps: exact-input
  reproduction, URL/query semantics, async ordering, 1-based pagination
  off-by-ones, cache key normalization and stale writes, swallowed
  errors.

    bench/run.sh --stream py-data --agent bench/agents/slag-fix.sh
    bench/run.sh --stream js-web --agent bench/agents/noop.sh

`--stream` swaps `bench/issues/` for `bench/streams/<domain>/` and runs
tasks in `order`. The runner carries nothing between tasks: each task
still gets a fresh repo copy and a lone ISSUE.md temp dir. An agent that
wants to carry lessons must do so itself. `--tasks`, `--runs`,
`--timeout`, `--out`, `--verify-fixtures`, and `--leak-check` all work
with `--stream`.

### Weak-executor baseline

The stream baseline uses slag with a weak, cheap base model and one
attempt per task (`SLAG_FIX_TRIES=1`) with the fix left in the working
tree (`SLAG_FIX_NO_COMMIT=1`), so the number measures the model, not
retries on the plan model. Both are opt-in; without them the adapter runs
`slag fix` as a user would:

    SLAG_BIN=slag-rs/target/debug/slag SLAG_PROVIDER=mistral \
      SLAG_MODEL_BASE=ministral-3b-latest SLAG_FIX_TRIES=1 SLAG_FIX_NO_COMMIT=1 \
      bench/run.sh --agent bench/agents/slag-fix.sh --stream py-data \
      --out bench/results/stream-baseline-py-data.json

Chosen executor: `mistral` / `ministral-3b-latest`. Candidates tried:
`ministral-8b-latest` passed 7/8 on js-web (too strong, no headroom);
`openai` / `gpt-4.1-nano` passed 5/8 on py-data and 4/8 on js-web
(slightly strong and uneven); `ministral-3b-latest` passes 2/8 on each
stream, so lessons from earlier tasks have room to show.

Baseline (slag v2.9.0, `slag fix --no-commit --tries 1`,
ministral-3b-latest, 2026-10-19):

    stream    task                  result   wall(s)
    py-data   01-csv-bom            PASS     26
    py-data   02-money-round        FAIL     100
    py-data   03-tz-filter          PASS     25
    py-data   04-unicode-dedupe     FAIL     92
    py-data   05-csv-quoted-newline FAIL     72
    py-data   06-float-total        FAIL     37
    py-data   07-dst-schedule       FAIL     55
    py-data   08-csv-chunk-boundary FAIL     43
    js-web    01-query-decode       FAIL     15
    js-web    02-cache-params-order FAIL     115
    js-web    03-pager-offbyone     PASS     37
    js-web    04-async-order        FAIL     28
    js-web    05-url-join           PASS     17
    js-web    06-fetch-error-swallow FAIL    139
    js-web    07-cache-stale-race   FAIL     57
    js-web    08-cursor-pagination  FAIL     6

    py-data: 2/8   js-web: 2/8   total: 4/16 (25%)

Baseline JSONs: `results/stream-baseline-py-data.json`,
`results/stream-baseline-js-web.json`.

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

`<issue_file>` is a copy of ISSUE.md in a temp dir of its own, so the
path never sits beside `hidden/` or `reference.patch`. Adapters receive
no environment variable pointing into `bench/issues/`, with one
exception: when the adapter's basename is `reference.sh`, the runner
sets `BENCH_REFERENCE_PATCH` to the task's patch. Anything the adapter
prints goes to the run log. Bundled adapters: `noop.sh` (does nothing),
`probe.sh` (leak check), `reference.sh` (applies the reference patch),
`codex.sh`, `fx.sh`, `slag-fix.sh` (runs `slag fix` as a user would;
SLAG_BIN picks the binary; opt in with SLAG_FIX_TRIES, SLAG_FIX_NO_COMMIT=1
and SLAG_FIX_ARGS).

## Instrument checks

    bench/run.sh --verify-fixtures
    bench/run.sh --verify-fixtures --stream py-data
    bench/run.sh --verify-fixtures --stream js-web
    bench/run.sh --leak-check

Every task's hidden suite must fail on the untouched repo and pass
after `reference.patch` is applied. The leak check runs the probe
adapter through the normal plumbing; it fails if the environment or the
handed-over paths mention `hidden`, `reference.patch`, or
`bench/issues` (`--verify-fixtures` runs it too). Additionally, the
noop adapter must score 0/8 and the reference adapter 8/8, on the
default task set and on each stream:

    bench/run.sh --agent bench/agents/noop.sh
    bench/run.sh --agent bench/agents/reference.sh
    bench/run.sh --stream py-data --agent bench/agents/noop.sh
    bench/run.sh --stream py-data --agent bench/agents/reference.sh

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
