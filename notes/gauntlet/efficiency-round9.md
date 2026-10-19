# Efficiency, round 9

Slag now keeps verification instruments under gitignored `.slag/checks/`. Product suites work without that directory. Reference comparisons still check immutable input and their own successful command results.

Builders can write instrument files there. They cannot rewrite coordinator documents. File guards reject symlinks and traversal. Coordinator fixture writes also reject symlink paths and use atomic replacement, which preserves external hardlink targets.

Contracts derive stdin conventions from stream operations, rather than generic file, text, CSV, or like keywords. Unix filters retain stdin and dash input. Scalar duration parsers take positional values. Python package CLIs retain entry points. Named behavior remains the first acceptance check.

A reference product launch no longer needs the same prefix as its probe wrapper. Invalid reference contracts receive one correction call with the precise validation error. If correction fails, full planning receives the original commission. A valid reference with another invalid contract field receives full scope repair. Contract shape errors no longer stop the commission.

CSV contracts also require row validation before JSON output. The live converter stages output in private temporary storage and emits nothing for malformed rows.

All seven live runs used OpenAI gpt-6.1-sol for work and planning, with --temper 1. The runs overlapped. Each run had a $0.19 cap. Costs come from engine usage events and remain estimates.

| Run | Seconds | Tokens | Estimated USD | Verdict |
|---|---:|---:|---:|---|
| wc-1 | 128.3 | 134,447 | $0.161733 | FORGED, exit 0 |
| wc-2 | 124.1 | 121,433 | $0.139617 | FORGED, exit 0 |
| wc-3 | 115.2 | 112,173 | $0.141312 | FORGED, exit 0 |
| csv | 148.9 | 136,980 | $0.163396 | FORGED, exit 0 |
| node | 160.0 | 114,933 | $0.124208 | FORGED, exit 0 |
| tail | 142.8 | 139,471 | $0.173353 | FORGED, exit 0 |
| durations | 96.5 | 109,948 | $0.102161 | FORGED, exit 0 |

Total live spend: **$1.0057802**. All three wc runs completed without a reference contract crash. No extra live attempts ran.

The wc runs each contain four tracked files. The duration CLI takes its duration positionally and exposes no file or stdin interface. Its public library API returns 5,400 for 1h30m and 90 for 90s.

The independent checker cloned committed HEAD and created a clean Python runtime without pip. Product suites ran without installation or `.slag/`. It checked 332 cases across the seven repositories.

| Run | Fresh-clone suite | Independent checks |
|---|---:|---|
| wc-1 | 6 tests, exit 0 | 96 checks: individual and combined flags, system-wc boundaries, stdin, dash, bounded reads, and tracked-file hygiene. |
| wc-2 | 6 tests, exit 0 | The same 96 checks passed. |
| wc-3 | 7 tests, exit 0 | The same 96 checks passed. |
| CSV | 7 tests, exit 0 | 13 checks: BOM, quoted newlines, pretty output, module launch, stdin, dash, errors with empty stdout, and tracked-file hygiene. |
| Node | 7 tests, exit 0 | 8 checks: NFC counts, positional N, default N, stdin, dash, help, and tracked-file hygiene. |
| Tail | 6 tests, exit 0 | 13 checks: last-N before filtering, zero, +N, regex errors, stdin, dash, appends, partial records, and tracked-file hygiene. |
| Durations | 6 tests, exit 0 | 10 checks: public API, positional values, zero, invalid input, help, absence of file/stdin extras, and tracked-file hygiene. |

Each product repository has clean git status. Tracked files contain no probe scripts, reference fixtures, coordination files, virtual environments, or egg-info. Internal ignore rules remain local. Warden verdicts cite their own successful inspections. The package checks also ran installed console scripts from temporary copies.

The final audit tightened fixture writes and normalized reference paths after the live runs. Native regressions verify those path guards. The final full suite passed **1,251 tests**, with one ignored. Cargo returned exit 0. Clippy returned exit 0 with warnings denied.

Exact git ls-files output follows.

`git -C /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/wc-1 ls-files`

```text
.gitignore
README.md
test_wc.py
wc.py
```

`git -C /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/wc-2 ls-files`

```text
.gitignore
README.md
test_wc.py
wc.py
```

`git -C /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/wc-3 ls-files`

```text
.gitignore
README.md
test_wc.py
wc.py
```

`git -C /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/csv ls-files`

```text
.gitignore
README.md
csvjson/__init__.py
csvjson/__main__.py
pyproject.toml
tests/__init__.py
tests/test_cli.py
```

`git -C /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/node ls-files`

```text
.gitignore
README.md
cli.cjs
package.json
test/words.test.cjs
words.cjs
```

`git -C /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/tail ls-files`

```text
.gitignore
README.md
logtail/__init__.py
logtail/__main__.py
pyproject.toml
tests/__init__.py
tests/test_cli.py
```

`git -C /Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/durations ls-files`

```text
.gitignore
README.md
duration_parser/__init__.py
duration_parser/__main__.py
pyproject.toml
test_duration_parser.py
```

Per-phase usage:

| Run | Phase | Input tokens | Output tokens | Total tokens | Calls |
|---|---|---:|---:|---:|---:|
| wc-1 | surveyor | 1,026 | 560 | 1,586 | 1 |
| wc-1 | smith | 110,184 | 3,649 | 113,833 | 17 |
| wc-1 | warden | 17,758 | 1,270 | 19,028 | 3 |
| wc-2 | surveyor | 1,026 | 482 | 1,508 | 1 |
| wc-2 | smith | 96,681 | 3,715 | 100,396 | 16 |
| wc-2 | warden | 18,361 | 1,168 | 19,529 | 3 |
| wc-3 | surveyor | 1,026 | 428 | 1,454 | 1 |
| wc-3 | smith | 87,073 | 3,823 | 90,896 | 14 |
| wc-3 | warden | 18,597 | 1,226 | 19,823 | 3 |
| csv | surveyor | 2,102 | 831 | 2,933 | 2 |
| csv | smith | 94,820 | 3,891 | 98,711 | 17 |
| csv | warden | 34,556 | 780 | 35,336 | 6 |
| node | surveyor | 881 | 311 | 1,192 | 1 |
| node | smith | 83,476 | 3,132 | 86,608 | 17 |
| node | warden | 26,549 | 584 | 27,133 | 5 |
| tail | surveyor | 883 | 534 | 1,417 | 1 |
| tail | smith | 99,041 | 4,328 | 103,369 | 17 |
| tail | warden | 33,435 | 1,250 | 34,685 | 5 |
| durations | surveyor | 884 | 512 | 1,396 | 1 |
| durations | smith | 72,882 | 2,383 | 75,265 | 15 |
| durations | warden | 32,377 | 910 | 33,287 | 6 |

Exact live commands, each from its fresh repository:

```sh
slag "write wc.py that counts lines, words and bytes of a file like wc, with a unittest suite" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
# Run the wc command three times in separate repositories.
slag "a small Python CLI that converts CSV to JSON with a --pretty flag, in a package with tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a tiny Node CLI that prints the top N most frequent words of a text file, with node:test tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a Python CLI that tails a log file and prints only lines matching a regex, with a --follow flag and tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a small Python library and CLI that parses durations like 1h30m or 90s into seconds, with tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
```

The driver used the worktree's `slag-rs/target/debug/slag` binary. Evidence lives under `/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/`.

- `<run>.json`: usage, model names, calls, elapsed time, and process exit code.
- `independent-verified/<run>.json`: fresh-clone suites and independent checks.
- `repository-checks.json`: tracked files, status, history, and private instrument paths.
- Each repository's `.slag/logs/engine-*.jsonl`: tool results, usage, and verdict.

Verification commands:

```sh
export TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r9/tmp
export CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
cargo test --offline --manifest-path slag-rs/Cargo.toml
cargo clippy --offline --manifest-path slag-rs/Cargo.toml --all-targets -- -D warnings
cargo build --offline --manifest-path slag-rs/Cargo.toml
R9_SOURCE_SUFFIX=-1 python3 slag-rs/target/round9-independent.py wc
R9_SOURCE_SUFFIX=-2 python3 slag-rs/target/round9-independent.py wc
R9_SOURCE_SUFFIX=-3 python3 slag-rs/target/round9-independent.py wc
python3 slag-rs/target/round9-independent.py csv
python3 slag-rs/target/round9-independent.py node
python3 slag-rs/target/round9-independent.py tail
python3 slag-rs/target/round9-independent.py durations
git diff --check
git -C /Users/stas/Playground/slag status --short
```

The independent checker preserves evidence and refuses to overwrite its clone directories.

Regression tests:

- `scalar_and_file_tools_do_not_inherit_stream_filter_interfaces`
- `scope_keeps_harness_instruments_out_of_product_tests`
- `a_reference_product_launch_does_not_need_to_match_a_probe_wrapper`
- `reference_product_paths_cannot_hide_coordinator_probes`
- `reference_contract_repair_reports_the_exact_reason_and_falls_back`
- `a_valid_reference_with_invalid_proof_gets_a_full_scope_repair`
- `an_invalid_reference_contract_falls_back_with_the_original_goal`
- `builder_can_write_instruments_but_not_coordinator_documents`
- `instrument_writes_reject_symlinks_into_coordinator_files`
- `direct_fixtures_stay_under_the_instrument_directory`
- `direct_fixtures_reject_symlinks_and_replace_hardlinks`

Changed files:

- `slag-rs/src/pipeline/direct.rs`: tool-kind conventions, instrument prompts, reference diagnostics, and tests.
- `slag-rs/src/pipeline/direct/reference.rs`: private reference fixtures and independent product launches.
- `slag-rs/src/pipeline/surveyor.rs`: bounded correction and full-planning fallback.
- `slag-rs/src/pipeline/mod.rs`: private fixture writes and acceptance instructions.
- `slag-rs/src/engine/agent.rs`: instrument write exception and coordinator protection.
- `slag-rs/src/engine/prompt.rs`: separate instruments from product tests.
- `slag-rs/src/flux.rs`: shared instrument, interface, and converter requirements.
- `slag-rs/tests/commission_efficiency.rs`: fallback integration regression.
- `README.md` and `CLAUDE.md`: instrument layout and tool-specific interfaces.
- `notes/gauntlet/efficiency-round9.md`: this report.
