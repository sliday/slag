# Commission verification, round 5

The command matcher now accepts known test-output flags while preserving the program and suite selection. Generic reference probes compare empty text and an unterminated final record with the named tool. Both live commissions ended without a pass. Their independent CLI checks exposed separate artifact gaps.

## Command grounding

The critic's session ran `python3 -m unittest discover --verbose` and passed two tests. The contract named `python3 -m unittest discover`. Exact argv equality rejected the successful inspection.

The matcher now normalizes known output flags for unittest, pytest, and Node's test runner. It retains executable identity and selection arguments. It recognizes unittest's default discovery entry. It keeps CLI arguments exact. Unknown options retain exact matching.

Allowed examples:

- Unittest `-v`, `--verbose`, `-q`, `--locals`, buffering, and duration reports.
- Pytest verbosity, traceback style, capture settings, and color settings.
- Built-in Node test reporters.

Refused examples:

- Extra test names, directories, patterns, or `-k` filters.
- Node `--test-name-pattern` and `--test-only`.
- A different Python executable, help/version commands, shell pipelines, and custom reporter modules.
- Node script arguments that resemble test-runner flags.

The evidence keeps the actual command. The matcher does not rewrite recorded inspections or accept failed runs. Zero-test checks cover the allowed Node reporters, including dot and JUnit. Those checks do not treat ordinary CLI XML as a Node test report.

## Reference boundaries

The direct contract binds a product launch to the named reference executable. Both programs receive the same input. The generated checks compare their exit status and output. The planner chooses byte comparison or whitespace-separated field comparison according to the requested behavior.

The coordinator creates two text inputs before construction: empty content and an unterminated final record. Each probe verifies the fixture content before execution. A model cannot remove the edge case by adding a final newline. The implementation contains no wc-specific behavior or expected counts.

Real read-only tests cover `cat` and `wc`. They accept matching programs and reject an added newline, an incorrect line count, a changed fixture, and matching stdout with a wrong exit status. The installed `/usr/share/man/man1/wc.1` excludes characters after the final newline from its line count. The oracle uses the installed reference program rather than a copied numeric expectation.

The live wc scope omitted the reference descriptor twice and fell back to full planning. Its initial JSON example and repair instructions still listed only the original four fields. I fixed those schemas after the live run. An omitted or invalid mapping now receives one dedicated mapping request. If that mapping remains invalid, the direct route stops before construction instead of dropping the comparison. The mock provider verifies recovery and the two-call bound. This final recovery change has no second live run, as requested.

## Live runs

Each exact commission ran once, in a fresh directory, with `--temper 1 --provider openai`. The provider returned `gpt-4.1-mini-2025-04-14`. Each run had a $0.35 run cap and a $0.30 ingot cap. Neither run exhausted those caps.

| Commission | Tokens | Seconds | Estimated cost | Verdict | Independent CLI check |
|---|---:|---:|---:|---|---|
| CSV to JSON | 224,280 | 103.96 | $0.0589848 | GOAL NOT MET | Correct quoted CSV conversion and --pretty output |
| wc.py | 278,831 | 140.44 | $0.0896000 | GOAL UNVERIFIED | Empty and terminated inputs match; unterminated inputs do not |

CSV's declared default discovery command found zero tests. Discovery with an explicit `tests` directory ran three tests and passed. The warden rejected the original declared check. This failure did not come from an added output flag.

Wc's final warden could not run its suite under the read-only policy because the tests created temporary files. An independent input `one two\nthree` produced `2 3 13`; system wc produced `1 3 13`. Empty input and a trailing-newline input matched system wc. A second Unicode input confirmed the same line-count defect.

The independent CSV check used quoted commas and escaped quotes. Both compact and pretty JSON matched Python's CSV parser. Pretty output contained indentation. I did not edit either live artifact to make a check pass.

Live artifacts, stdout, stderr, engine logs, and JSON summaries reside under `/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r5`. Independent results and exact argv reside in `independent.json`. No provider calls followed these two commissions.

Total estimated live spend: **$0.1485848**, below $0.80.

## Phase tokens

These totals come from the engine logs. Phase durations in the tape can cross failed-session boundaries, so this table reports token totals only.

| Run | Phase | Input | Output | Total | Calls |
|---|---|---:|---:|---:|---:|
| csv | surveyor | 406 | 125 | 531 | 1 |
| csv | smith | 173,164 | 8,441 | 181,605 | 42 |
| csv | plan | 31,917 | 233 | 32,150 | 5 |
| csv | warden | 9,135 | 859 | 9,994 | 3 |
| wc | surveyor | 11,242 | 2,608 | 13,850 | 5 |
| wc | warden | 14,722 | 585 | 15,307 | 5 |
| wc | founder | 21,897 | 2,029 | 23,926 | 4 |
| wc | smith | 215,007 | 10,741 | 225,748 | 39 |

## Verification

The command and reference regressions failed before implementation and passed after the fixes. Cargo returned exit code 0: 1,146 tests passed and one paid provider smoke test stayed ignored. Clippy returned exit code 0 with warnings denied. The post-run scope recovery and reporter guards received mock and native regression coverage; neither commission ran again.

Relevant tests:

- `test_output_flags_preserve_grounded_suite_evidence`
- `reference_probes_detect_unterminated_record_errors_without_tool_specific_rules`
- `reference_count_fields_reject_an_unterminated_line_mismatch`
- `a_named_reference_requires_the_actual_reference_and_artifact_launch`
- `an_omitted_reference_gets_a_bounded_mapping_request`
- `an_invalid_reference_contract_stops_before_forging`
- `zero_test_reports_cannot_support_a_required_suite_pass`

Exact verification commands from the worktree root:

```sh
TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r5/tmp CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test --offline --manifest-path slag-rs/Cargo.toml
TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r5/tmp CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo clippy --offline --manifest-path slag-rs/Cargo.toml --all-targets -- -D warnings
git diff --check
git -C /Users/stas/Playground/slag status --short
```

Live commands ran inside isolated scratch directories:

```sh
slag "a small Python CLI that converts CSV to JSON with a --pretty flag, in a package with tests" --temper 1 --provider openai
slag "write wc.py that counts lines, words and bytes of a file like wc, with a unittest suite" --temper 1 --provider openai
```

## Coordination files

A `.slag/` directory suits coordination state. I deferred the migration because path constants, prompt readers, snapshots, proofs, and resume still use the root layout. A partial move would break compatibility with existing projects. New commissions retain the root layout until a complete migration can preserve existing path behavior.

## Files changed

- `notes/gauntlet/efficiency-round5.md`
- `slag-rs/src/flux.rs`
- `slag-rs/src/pipeline/direct.rs`
- `slag-rs/src/pipeline/direct/reference.rs`
- `slag-rs/src/pipeline/mod.rs`
- `slag-rs/src/pipeline/surveyor.rs`
- `slag-rs/src/pipeline/warden/grounding.rs`
- `slag-rs/tests/commission_efficiency.rs`

`engine/provider.rs` stayed unchanged. Its source hash remained `097ec6de1843349b15b7ae81ef504357b657fae1`. The main checkout status stayed empty.
