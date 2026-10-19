# Commission quality and verification, round 4

The Node commission passed. Wc and CSV cracked under final spend caps. Full live acceptance remains incomplete.

The direct route now derives a checklist before construction. The blueprint, smith task, and bar share that checklist. The scope response names a full test suite and concrete CLI probes. Code adds the package, CLI, and test requirements that the sentence names when the model omits them. Named flags must appear in the criteria and probe arguments.

The warden must cite its own successful suite and probe runs before it can pass. The grounding code matches actual commands against declared commands. Source reads alone cannot satisfy this contract. The forge runs direct proofs under the same read-only profile, so a smith can repair tests that write fixtures before the final review.

Builders receive instructions to test actual CLI results, including named flags. Package requirements include metadata and a suitable launch method. The prompt excludes fixed test counts, dependency bans, and extra features.

The generated ignore file includes caches and build directories for the detected stack. Declared builder checks also use the read-only profile, so fixture writes fail during construction. Verification removes newly created scratch files before commit. It preserves supplied fixtures. The cleanup refuses symlink parents and external paths. Ignore updates refuse symlinks. A filesystem snapshot supports old projects without a Git repository.

Unreadable model text goes into `logs/warden-raw-*.log`. The agent saves the original text before normalization.

## Cost changes

The baseline used production commit `f7dec2a5c7b7d866b00d051770174cc220e0f8a0`, OpenAI, and `gpt-4.1-mini-2025-04-14`. It used 45,063 tokens in 24.67 seconds. The smith used 89% of its tokens. It repeated two failing tests before it passed. The source-only warden missed incorrect Unix line-count semantics.

| Baseline phase | Input | Output | Total | API seconds | Calls |
|---|---:|---:|---:|---:|---:|
| Scope | 165 | 13 | 178 | 1.356 | 1 |
| Smith | 37,998 | 1,989 | 39,987 | 18.394 | 10 |
| Warden | 4,799 | 99 | 4,898 | 1.569 | 2 |
| Total | 42,962 | 2,101 | 45,063 | 21.319 | 13 |

One recent tool round caused repeated edits during an experiment. The final implementation retains two rounds. Direct grade-1 work has a 16-turn budget, with an explicit verification command in its final fallback. The agent accepts a lowercase `cmd:` label in that fallback. Direct repair heats preserve product edits after failed checks. Provider-error rollback stays unchanged. The combined proof checks the suite and CLI probes before commit. Wardens receive no replay of coordinator proof logs. They must run their own checks.

Fenced scope JSON and harmless output redirections triggered full planning during experiments. The parser now accepts one outer JSON fence and exact standard device targets. Other output targets still fail validation. The shorter scope prompt separates route selection from product requirements. One bounded repair of an invalid direct contract can avoid full planning. False scope choices and provider errors receive no repair. Explicit `node:test` requests use `node --test`. A zero-test Python or TAP report cannot support a required pass.

## Evidence

Live artifacts and raw logs reside under `/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r4`. Each run has a JSON summary, stdout, stderr, engine logs, and a tape. All runs use fresh directories. The driver stops on provider quota errors and enforces a wall-time limit.


## Final live runs

Each run used its exact requested prompt with `--temper 1 --provider openai`. The model was `gpt-4.1-mini-2025-04-14`. Each run used a fresh directory. The final caps reserved the remaining live budget: $0.018 per run and $0.0153 per ingot. An in-flight call can exceed its cap.

| Commission | Tokens | Wall seconds | Estimate | Result | Independent CLI check |
|---|---:|---:|---:|---|---|
| wc.py | 75,632 | 38.96 | $0.021522 | CRACKED; no warden verdict | Runs, but counts an unterminated last line incorrectly |
| CSV to JSON | 72,934 | 36.96 | $0.021773 | CRACKED; no warden verdict | Conversion and --pretty run; package installation fails |
| Node top words | 51,842 | 29.06 | $0.014430 | FORGED; grounded warden PASS | Runs; exact top-N output and five node:test tests pass |

Wc and CSV reached their ingot caps before a successful proof. Later heats still spent small amounts before they stopped. Their last assay displayed CRACKED. Neither run reached an artifact verdict. Budget exhaustion still causes wasted retry calls; that remains unresolved.

The final Node run passed on heat 1. Its warden ran `node --test` and both declared CLI probes. The evidence cites successful calls from that session. A separate CLI check returned `red 3` and `blue 2` for the two most frequent words.

The wc CLI returned `2 4 16` for a file with one newline and an unterminated final line. System `wc` returned `1 4 16`. The CSV CLI produced correct indented JSON through `python3 -m package.cli --pretty --input FILE --output FILE`. Its repository contains `setup.cfg` but lacks `setup.py` and `pyproject.toml`. Pip rejected the package as not installable. Neither artifact received a pass.

Manual results and exact command arguments reside in `manual-final.json` in the job directory. The final small budget limits conclusions about successful wc or CSV cost. These results do not establish a cost win for either commission. First-pass Python test reliability remains unresolved.

## Final wc phase table

API seconds sum each model-call interval from the tape. This table uses engine-session boundaries because failed sessions can omit a finish event.

| Phase | Input tokens | Output tokens | Total tokens | API seconds | Calls |
|---|---:|---:|---:|---:|---:|
| Scope | 406 | 105 | 511 | 2.922 | 1 |
| Smith heat 1 | 60,825 | 2,067 | 62,892 | 22.727 | 14 |
| Smith heat 2 | 2,914 | 750 | 3,664 | 5.580 | 1 |
| Smith heat 3 | 2,914 | 602 | 3,516 | 4.878 | 1 |
| Re-smelt | 5,001 | 48 | 5,049 | 1.017 | 1 |
| Total | 72,060 | 3,572 | 75,632 | 37.124 | 18 |

Smiths used 70,072 tokens, 92.6% of the final wc total. The code bounds tool loops, preserves repair progress, and exposes read-only failures sooner. Python tests still created fixtures or used temporary directories despite the contract. The final wc run failed, so its cost cannot replace the successful baseline figure.

## Spend ledger

This table includes the baseline and superseded experiments. Stopped runs retain their engine logs. Failed experiments account for most of this round's spend.

| Run | Tokens | Wall seconds | Estimate | Exit |
|---|---:|---:|---:|---:|
| csv-delivery | 192,748 | 81.46 | $0.048203 | 0 |
| csv-final | 747,553 | 180.08 | $0.222309 | -15 |
| csv-final-build | 72,934 | 36.96 | $0.021773 | 1 |
| csv-r4 | 264,536 | 102.47 | $0.061648 | 1 |
| csv-release | 298,288 | 145.93 | $0.086397 | -15 |
| csv-verified | 938,691 | 126.17 | $0.252960 | 1 |
| node-delivery | 324,737 | 148.93 | $0.085878 | -15 |
| node-final | 546,137 | 180.10 | $0.180440 | -15 |
| node-final-build | 51,842 | 29.06 | $0.014430 | 0 |
| node-r4 | 165,121 | 75.15 | $0.042492 | 0 |
| node-release | 31,363 | 26.98 | $0.011562 | 1 |
| node-verified | 220,393 | 105.17 | $0.070186 | 0 |
| wc-baseline | 45,063 | 24.67 | $0.015516 | 0 |
| wc-delivery | 366,783 | 138.43 | $0.093278 | 1 |
| wc-final | 371,835 | 180.10 | $0.103996 | -15 |
| wc-final-build | 75,632 | 38.96 | $0.021522 | 1 |
| wc-r4 | 210,324 | 98.63 | $0.061517 | 1 |
| wc-release | 283,933 | 148.95 | $0.081729 | -15 |
| wc-verified | 39,647 | 24.54 | $0.009470 | 1 |

Total estimated live spend: **$1.4853056**, below $1.50. No further provider calls followed the final three runs.

## Tests and commands

Cargo returned exit 0: 1,121 tests passed and one pre-existing paid provider smoke test stayed ignored. Clippy returned exit 0 with warnings denied. `git diff --check` passed.

Relevant regressions:

- `named_package_cli_and_tests_get_contracts_when_the_model_omits_them`
- `an_explicit_test_framework_controls_the_suite_command`
- `an_invalid_direct_contract_gets_one_repair_attempt`
- `scope_provider_errors_do_not_trigger_repair`
- `passing_evidence_must_cover_the_suite_and_each_cli_probe`
- `zero_test_reports_cannot_support_a_required_suite_pass`
- `direct_builder_checks_are_readonly_but_construction_stays_writable`
- `direct_proof_repairs_fixture_writes_before_sealing_the_ingot`
- `verification_scratch_stays_out_of_commits_and_product_fixtures_survive`
- `unreadable_review_keeps_the_exact_raw_response`
- `turn_limit_keeps_the_verification_command_in_the_final_text_request`

The hygiene tests also cover symlinked ignore files, symlink parents during cleanup, and projects without Git. After manual checks, I added Node's spec reporter format to the zero-test guard. The full Rust suite and Clippy passed again. This formatting case used unit tests; the live Node suite ran five tests.

Exact Rust verification commands from the worktree root:

```sh
TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r4/tmp CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test --offline --manifest-path slag-rs/Cargo.toml
TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r4/tmp CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo clippy --offline --manifest-path slag-rs/Cargo.toml --all-targets -- -D warnings
git diff --check
git -C /Users/stas/Playground/slag status --short
```

The live driver ran these commands inside fresh scratch directories:

```sh
slag "write wc.py that counts lines, words and bytes of a file like wc, with a unittest suite" --temper 1 --provider openai
slag "a small Python CLI that converts CSV to JSON with a --pretty flag, in a package with tests" --temper 1 --provider openai
slag "a tiny Node CLI that prints the top N most frequent words of a text file, with node:test tests" --temper 1 --provider openai
```

The driver used this worktree's `slag-rs/target/debug/slag` binary. It set `TMPDIR` to the job directory and isolated `SLAG_CONFIG_DIR`. Keys stayed in the inherited environment.

## Changed files

- `notes/gauntlet/efficiency-round4.md`
- `slag-rs/src/engine/agent.rs`
- `slag-rs/src/flux.rs`
- `slag-rs/src/pipeline/direct.rs`
- `slag-rs/src/pipeline/forge.rs`
- `slag-rs/src/pipeline/hygiene.rs`
- `slag-rs/src/pipeline/mod.rs`
- `slag-rs/src/pipeline/surveyor.rs`
- `slag-rs/src/pipeline/warden.rs`
- `slag-rs/src/pipeline/warden/grounding.rs`
- `slag-rs/src/smith/native.rs`
- `slag-rs/tests/commission_efficiency.rs`
- `slag-rs/tests/forge_loop.rs`
- `slag-rs/tests/local_hygiene.rs`

The main checkout status was empty. All writes stayed in this worktree or the authorized job directory.
