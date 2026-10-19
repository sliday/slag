# Commission efficiency, round 11

Round 10 precedes this change as commit `f997117`. The production merge reported “Already up to date.”

## Changes

The surveyor now supplies the sole list of conventional interfaces. The coordinator no longer appends another interface checklist. Builders and reviewers use the BAR for that list. A named log file does not imply stdin, dash input, `-n +N`, or packaging. Explicit package requests retain their entry points.

The contract puts named behavior first. Each behavior needs exact probes for its applicable boundaries. File readers cover empty input, unterminated final lines, and invalid bytes. Text transforms cover separators, punctuation, and Unicode. Retry helpers cover attempt limits, delay growth, jitter bounds, and finite caps.

Tail snapshots emit their final unterminated line. Follow buffers incomplete appends.

The reviewer checks these assertions in source and successful runs before secondary interfaces. Missing assertions or failed probes cannot pass. Grounding rules remain unchanged.

The builder prompt favors standard parsers and serializers. The live CSV implementation uses `json.dump(..., indent=2 if args.pretty else None)`. Existing Git history now prevents bootstrap staging and commits. The mock regression starts with user history and confirms its preservation.

## Live results

Each run used OpenAI gpt-6.1-sol for work and planning, with `--temper 1`. Runs overlapped. Dollar figures are estimates from usage events.

| Run | Seconds | Tokens | Estimated USD | Verdict |
| --- | ---: | ---: | ---: | --- |
| wc | 199.4 | 196,390 | $0.219310 | CRACKED, no warden verdict |
| wc-final | 190.5 | 174,027 | $0.248188 | CRACKED, no warden verdict |
| wc-verified | 140.6 | 232,381 | $0.257001 | CRACKED, no warden verdict |
| csv | 148.7 | 135,407 | $0.160843 | FORGED, warden pass |
| node | 115.9 | 115,275 | $0.133325 | FORGED, warden pass |
| tail | 156.8 | 145,114 | $0.164950 | FORGED, warden pass |
| durations | 96.4 | 101,555 | $0.116501 | FORGED, warden pass |
| slug | 55.1 | 37,452 | $0.044152 | FORGED, warden pass |
| retry | 101.1 | 108,348 | $0.124893 | FORGED, warden pass |

Total spend across all nine attempts: **$1.469163**, below $1.50.

The first wc contract contained a false numeric expectation: seven UTF-8 bytes counted as eight. The smith corrected product tests, but the inline proof repeated that false expectation. The run cracked. No successful verdict hid that failure.

I added general guidance to derive numeric expectations from executable references or builtin computations. Suite and probes now share an oracle instead of duplicated constants. The wc-final and wc-verified runs used that guidance. Six other runs preceded this prompt addition. A final guidance change also requires host-valid reference arguments for equivalent input modes. I did not test that last change live because the spend limit left $0.031.

Tail needed heat 2 because the first heat omitted its named probe script. The first two wc attempts exhausted three heats. The second wc attempt hit a test bug: splitting output also split a filename with spaces. Its repair then hit the imposed budget limit.

The last attempt used a $0.25 run cap and a $0.22 ingot cap. Other runs used $0.19 and $0.18 caps. The third wc contract passed literal - to native macOS wc, which rejects that operand. The smith corrected its product tests but could not change the coordinator’s inline proof. It exhausted its budget before delivery.

No wc attempt reached the warden. Semantic contract coverage still depends on model inspection; the parser cannot prove arbitrary expected values.

## Independent checks

For the six FORGED artifacts, I cloned committed HEAD and ran the documented full suites without installation or `.slag/`. Python checks used clean runtimes without pip. Independent checks used no provider calls. For wc, I checked a separate snapshot of the remaining working files. That snapshot is not a delivered commit or a warden pass.

| Artifact | Independent checks |
| --- | --- |
| wc working snapshot | 75 system-wc comparisons passed: empty, newline, unterminated, UTF-8, invalid bytes, individual/combined flags, file, stdin, and dash. The corrected suite passed four tests. Delivery remains CRACKED. |
| CSV | BOM, quoted newline, empty input, header-only input, unterminated row, pretty output, ragged rows, invalid encoding, and empty stdout on errors. |
| Node words | NFC, punctuation separators, empty input, unterminated word, invalid bytes, positional N, and N=0. |
| Tail | Last ten before filtering, empty input, unterminated matching/nonmatching lines, invalid UTF-8, -n, appends, and buffered partial lines. |
| Durations | Public API, 1h30m, 90s, zero, combined units, empty/invalid/signed input, and punctuation errors. |
| Slugifier | a.b.c → a-b-c; rock&roll → rock-roll; empty input, repeated separators, Unicode, and NFC. |
| Retry | One-attempt limit, rejection identity, exponential growth, full jitter, finite cap, 1,100 attempts without overflow, zero delay, and invalid options. |

Fresh-clone suites and independent evidence live in `slag-rs/target/round11/independent/*.json`. Wc working-file evidence lives in `wc-working.json`. Tracked-file lists contain product files and product tests. They contain no coordination documents or verification probes.

## Tail BAR

Exact BAR from the live tail commission:

```text
## THE BAR
Deliver the named behavior in the original commission.

## CHECKLIST
- [ ] Tail selects the final 10 lines before regex filtering; suite asserts empty file => '', 'a\nb\n' with '.' => 'a\nb\n', and lines 1–12 with '^(1|11)$' => '11\n'.
- [ ] Support -n N: suite asserts 'a\nb\nc\n' with -n 2 and '.' => 'b\nc\n'; -n 0 => ''. A finite snapshot of 'a\nb' with '^b$' emits 'b' without adding a newline.
- [ ] Regex searches each selected line: suite asserts 'cat\nscatter\ndog\n' with 'cat' => 'cat\nscatter\n', and '^z$' => ''. Invalid '[' exits nonzero with a useful regex error on stderr.
- [ ] --follow emits the initial filtered tail, then matching appended records. A bounded subprocess probe asserts initial 'hit\n', appended 'miss\nhit2\n', then partial 'hit3' produce 'hit\nhit2\n'; appending '\n' adds exactly 'hit3\n'.
- [ ] Read incrementally without whole-file reads, including during follow. Decode UTF-8 with replacement for invalid bytes; suite asserts bytes b'ok\n\xffhit\n' with 'hit' emit '�hit\n'.
- [ ] Launch as python3 logtail.py FILE REGEX, with optional -n N and --follow; subprocess tests cover these interfaces. --help exits 0 with usage; missing arguments, unreadable files, and invalid N exit nonzero with useful stderr diagnostics.
- [ ] Judge named behavior and boundary cases before interface conveniences. Use subprocess checks with exact expected results for the named behavior and flags. Run the stated CLI launch.
- [ ] Run a nonempty full test suite that exercises the commission's named behavior. Its documented command must pass in a fresh clone without prior installation.

REQUIRED RUN COMMANDS: ["python3 -B -m unittest","python3 -B .slag/checks/probe_cli.py","python3 -B .slag/checks/probe_cli.py --follow"]
FRESH CLONE TEST COMMAND: "python3 -B -m unittest"

## HOW TO INSPECT
Read the product and test source, then run the declared suite and CLI probes.
Run each required check in its own bash call. Known test output flags may differ. Preserve the program, suite selection, and CLI arguments. Cite those successful run IDs in EVIDENCE.
Check each named behavior and its listed mundane boundary probes first. Confirm their assertions in test source and successful runs. Missing assertions or failed probes cannot pass. Check secondary interfaces afterward. Source inspection alone cannot pass this bar.
Keep verification instruments and reference fixtures under .slag/checks/. Product tests must work without .slag/.
The reviewer can read and run instruments. Use private TMPDIR for scratch; keep the workspace read-only.
Judge the commission without extra naming, style, or test-count rules.
```

## Usage by phase

| Run | Phase | Input tokens | Output tokens | Total tokens | Calls |
| --- | --- | ---: | ---: | ---: | ---: |
| wc | surveyor | 1,252 | 983 | 2,235 | 1 |
| wc | smith | 175,713 | 6,321 | 182,034 | 24 |
| wc | plan | 12,090 | 31 | 12,121 | 1 |
| wc-final | surveyor | 1,289 | 589 | 1,878 | 1 |
| wc-final | smith | 160,915 | 2,733 | 163,648 | 19 |
| wc-final | plan | 8,451 | 50 | 8,501 | 1 |
| wc-verified | surveyor | 1,289 | 874 | 2,163 | 1 |
| wc-verified | smith | 215,680 | 3,602 | 219,282 | 22 |
| wc-verified | plan | 10,905 | 31 | 10,936 | 1 |
| csv | surveyor | 2,718 | 1,277 | 3,995 | 2 |
| csv | smith | 90,179 | 3,857 | 94,036 | 17 |
| csv | warden | 36,569 | 807 | 37,376 | 6 |
| node | surveyor | 1,107 | 605 | 1,712 | 1 |
| node | smith | 80,991 | 3,438 | 84,429 | 15 |
| node | warden | 28,537 | 597 | 29,134 | 5 |
| tail | surveyor | 1,109 | 599 | 1,708 | 1 |
| tail | smith | 120,646 | 4,377 | 125,023 | 25 |
| tail | warden | 17,621 | 762 | 18,383 | 3 |
| durations | surveyor | 1,110 | 668 | 1,778 | 1 |
| durations | smith | 81,496 | 2,301 | 83,797 | 15 |
| durations | warden | 15,086 | 894 | 15,980 | 3 |
| slug | surveyor | 1,101 | 397 | 1,498 | 1 |
| slug | smith | 18,498 | 921 | 19,419 | 5 |
| slug | warden | 15,897 | 638 | 16,535 | 4 |
| retry | surveyor | 1,106 | 577 | 1,683 | 1 |
| retry | smith | 83,243 | 2,729 | 85,972 | 17 |
| retry | warden | 20,196 | 497 | 20,693 | 4 |

## Verification

Final native Cargo suite: **1,266 passed, 0 failed, 1 ignored**, Cargo exit 0. Clippy passed with exit 0 and warnings denied.

```sh
export TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r11/tmp
export CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
cargo test --offline --manifest-path slag-rs/Cargo.toml
cargo clippy --offline --manifest-path slag-rs/Cargo.toml --all-targets -- -D warnings
cargo build --offline --manifest-path slag-rs/Cargo.toml
python3 slag-rs/target/round11-wc-working-check.py
python3 slag-rs/target/round11-independent.py csv
python3 slag-rs/target/round11-independent.py node
python3 slag-rs/target/round11-independent.py durations
python3 slag-rs/target/round11-independent-extra.py tail
python3 slag-rs/target/round11-independent-extra.py slug
python3 slag-rs/target/round11-independent-extra.py retry
git diff --check
git -C /Users/stas/Playground/slag status --short
```

Exact live argv, timing, usage, spend, and exits: `slag-rs/target/round11/live/<run>.json`. Tool evidence: each product repository’s `.slag/logs/engine-*.jsonl`. Native logs: `slag-rs/target/round11/test-verified.log` and `clippy-verified.log`. Independent scripts preserve their clone directories. Repeated checks need a new evidence directory.

## Regressions

- `conventional_interfaces_have_one_source_without_tail_scope_expansion`
- `surveyor_owns_conventional_interfaces_without_coordinator_duplicates`
- `scope_prioritizes_semantics_and_limits_convenience_interfaces`
- `numeric_probe_expectations_use_executable_oracles`
- `builder_and_warden_prioritize_semantics_and_isolate_installs`
- `direct_commission_protects_coordinator_state_and_seals_before_review`

## Changed files

- `slag-rs/src/pipeline/direct.rs`: sole interface policy, boundary prompts, numeric oracle guidance, and regressions.
- `slag-rs/src/flux.rs`: contract-scoped delivery rules, behavior-first inspection, and a regression.
- `slag-rs/src/pipeline/mod.rs`: BAR inspection order and bootstrap guard for existing history.
- `slag-rs/tests/commission_efficiency.rs`: existing-history integration coverage.
- `notes/gauntlet/efficiency-round11.md`: this report.
