Round 2 closes the reviewer write path and restores saved-gap repair before the next artifact review.

I kept `pipeline/fix.rs` unchanged. Its tool-free review still accepts explicit failures with a named gap.

Changes

- I restrict each warden session to `read_file` and `bash`. I bypass tool hooks during review.
- I run reviewer shell commands under `Sandbox::read_only()`, including temporary directories and tool caches.
- I refuse package installs, mutation commands, and background jobs. A denied mutation ends review before another verdict request.
- I fail closed when the host cannot provide confinement. `--no-sandbox` does not disable reviewer confinement.
- I keep existing sandbox constructors compatible. Linux uses read-only mounts or Landlock ABI 3; macOS uses Seatbelt.
- I load the last named goal gap before resume. The founder casts repair ingots before the next artifact review.
- I distinguish an unverified goal from a rejected artifact. Malformed reports print “the warden returned no readable verdict; goal unverified”.
- I record proof commands and output before review. Reviewers receive bounded evidence from up to eight proof logs.
- I preserve the retry budget after an unverified ingot review.
- I ask for unittest discovery on fresh Python projects. A named test module can pass while default discovery finds no tests.

I keep the warden independent through source inspection and a fresh conversation. The coordinator executes write-producing tests before review.
The reviewer can read those results and run independent checks that need no filesystem writes.

Final measurements

I used `openai::gpt-4.1-mini-2025-04-14`. Cerebras had no credit; the user authorized OpenAI or Mistral for this round.
The final fresh runs used isolated configuration directories and inherited provider keys. I did not print keys.
I measured wall time around the child process. I cross-checked engine tokens against tape tokens and counted each token event once.
Session times span the first context gauge through finish. Local overhead includes process startup, proofs, git, and reporting.

| Commission | Tokens | Wall seconds | Verdict | Bare unittest |
|---|---:|---:|---|---|
| Exact wc commission | 9,457 | 8.124 | FORGED, pass, exit 0 | 3 tests, exit 0 |
| Same four-file CSV commission | 11,781 | 10.550 | FORGED, pass, exit 0 | 3 tests, exit 0 |

| Phase / session | ID | Role | Prompt | Completion | Seconds |
|---|---|---|---:|---:|---:|
| wc scope | 20261017_112739_433766000 | surveyor | 158 | 13 | 0.871 |
| wc i1 heat 1 | 20261017_112740_347706000 | smith | 5,325 | 430 | 4.151 |
| wc artifact | 20261017_112744_624582000 | warden | 3,384 | 147 | 1.968 |
| wc local overhead | n/a | local | 0 | 0 | 1.134 |
| **wc total** | n/a | all | **8,867** | **590** | **8.124** |
| CSV scope | 20261017_112741_529242000 | surveyor | 236 | 17 | 0.719 |
| CSV i1 heat 1 | 20261017_112742_310239000 | smith | 6,308 | 916 | 7.578 |
| CSV artifact | 20261017_112750_007829000 | warden | 4,117 | 187 | 1.904 |
| CSV local overhead | n/a | local | 0 | 0 | 0.349 |
| **CSV total** | n/a | all | **10,661** | **1,120** | **10.550** |

These runs omit blueprint negotiation, founder negotiation, duel, and re-smelt. The scope check routes each small commission to one ingot.

PEP 668 evidence

I seeded a tiny setuptools package with a passing greet test and an unsafe global installation instruction.
Homebrew Python 3.14 rejected `python3 -m pip install --no-deps --no-build-isolation .` with `externally-managed-environment`.
I preserved that real failure in `logs/initial-install.log`. I did not bypass or remove the PEP 668 marker.
The bar requires a project `.venv`, an installed package, safe documentation, and a read-only metadata/import check.

The first live review returned a named installation gap: 26,782 tokens, 18.496 seconds, exit 1.
The original `--temper 2 resume` cast and forged i2 before artifact round 2: 73,708 tokens, 33.559 seconds.
The builder added documentation and an install script. It did not create the required installed environment.
The artifact warden rejected the absent `.venv`. It did not edit the package or install dependencies.
This run proves repair scheduling and honest rejection. It does not prove a repaired package installation.

| Phase / session | ID | Role | Prompt | Completion | Seconds |
|---|---|---|---:|---:|---:|
| Initial blueprint review | 20261017_112242_378577000 | warden | 4,736 | 146 | 1.929 |
| Blueprint correction | 20261017_112244_336640000 | surveyor | 5,822 | 1,461 | 8.981 |
| Initial artifact review | 20261017_112253_357379000 | warden | 14,008 | 609 | 7.331 |
| Initial local overhead | n/a | local | 0 | 0 | 0.255 |
| **Initial total** | n/a | all | **24,566** | **2,216** | **18.496** |
| Resume founder | 20261017_112322_335970000 | founder | 23,886 | 944 | 9.014 |
| Resume i2 heat 1 | 20261017_112331_403290000 | smith | 16,441 | 1,286 | 9.940 |
| Resume ingot review | 20261017_112341_405898000 | warden | 16,814 | 607 | 7.349 |
| Resume artifact round 2 | 20261017_112348_872444000 | warden | 13,311 | 419 | 5.536 |
| Resume local overhead | n/a | local | 0 | 0 | 1.720 |
| **Resume total** | n/a | all | **70,452** | **3,256** | **33.559** |

I also replayed the saved real gap against the final binary. I restored the initial package and copied its real failure log and verdict.
The seed commit in the original scratch repository is `25ffebd1d518f3ef172ae57605591bed1b997c0c`.

One final replay stopped as unverified after an ingot reviewer attempted to create `.venv`; the sandbox denied that write.
Another replay cracked on a generated proof with a doubled backslash in its regex: 260,392 tokens, 107.966 seconds.
The builder's CMD passed; the distinct proof failed. The coordinator kept both outputs and never declared success.
This exposes a remaining generated-proof/escaping risk. I did not change the S-expression parser in this lane.

Verification

`CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test` completed with cargo exit 0: 1,053 passed, one ignored.
The `slag fix` parser compatibility test also passed. I did not alter the lead's `review_gap` fix.
The real OS sandbox tests completed outside the host sandbox: four passed, exit 0.

Key regression tests:

- `temper_resume_forges_the_prior_gap_before_the_next_artifact_review`
- `unreadable_artifact_verdict_reports_an_unverified_goal`
- `forbidden_warden_mutations_cannot_produce_an_accepted_pass`
- `warden_sessions_cannot_pass_after_a_forbidden_mutation`
- `readonly_review_mutation_taints_the_session_before_a_later_pass`
- `readonly_sandbox_blocks_workspace_and_machine_writes`
- `readonly_profiles_have_no_writable_filesystem_trees`
- `readonly_review_shell_runs_probes_or_fails_closed`
- `saved_goal_gaps_accept_both_ledger_headers_and_ignore_unverified_results`
- `an_explicit_unverified_report_keeps_the_inspection_limit`

The mutation tests cover unoffered `edit_file`, direct shell `touch`, package installation, and Python temporary-file creation.
The native warden test covers document/bar sessions as well as artifact review.

Exact verification commands from the worktree:

```sh
cd slag-rs
CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test
CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test readonly_
```

The second command needs a host that permits slag to start its own OS sandbox.
Inside Codex's outer sandbox, slag refuses nested confinement and leaves review unverified.

```sh
BASE=/private/tmp/slag-warden-r2-3yx6gkfi
SLAG=/Users/stas/Playground/slag/.claude/worktrees/lane-efficiency/slag-rs/target/debug/slag
cd "$BASE/wc-release-check"
python3 -m unittest
cd "$BASE/csv-release-check"
python3 -m unittest
cd "$BASE/pep668"
"$SLAG" --temper 1 resume --provider openai
"$SLAG" --temper 2 resume --provider openai
```

The PEP commands describe the recorded sequence. A repeat needs the initial scratch snapshot, because resume persists verdicts and ingots.
The JSON metrics preserve the exact wc/CSV commission arguments and the provider model for each session.

I also ran `CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo clippy --all-targets -- -D warnings`.
It exited 101 on existing warnings, including deprecated test helpers and dead code. I left unrelated lint changes out.

Changed files:

- `slag-rs/src/engine/agent.rs`
- `slag-rs/src/engine/events.rs`
- `slag-rs/src/engine/sandbox.rs`
- `slag-rs/src/flux.rs`
- `slag-rs/src/pipeline/forge.rs`
- `slag-rs/src/pipeline/mod.rs`
- `slag-rs/src/pipeline/warden.rs`
- `slag-rs/src/render/echo.rs`
- `slag-rs/src/smith/native.rs`
- `slag-rs/tests/commission_efficiency.rs`
- `notes/gauntlet/warden-round2-results.md`
- `notes/gauntlet/warden-round2-metrics.json`

Additional final-build PEP checks

I used another fixture with safe documentation and a missing actual installation.
Round 1 named the missing `.venv` and distribution: 13,935 tokens, 13.741 seconds, goal not met.
Resume cast two repair ingots and ran the builders before any artifact round.
The founder invented a nested package path and a script constraint. The builders did not satisfy those proofs.
The run reached its $0.18 cap and returned goal unverified. It did not print FORGED.
It used 886,863 tokens. The tape spans 127.212 seconds; parallel sessions broke the driver's session-time aggregation.
I recovered its token total from engine logs and cross-checked it against the tape.
A final minimal fixture caused the reviewer to attempt installation itself. The read-only policy blocked it before a pass.
That review used 6,458 tokens and 3.730 seconds and returned goal unverified.

| PEP check | Tokens | Seconds | Outcome |
|---|---:|---:|---|
| Original saved-gap resume | 73,708 | 33.559 | i2 forged; artifact round 2 rejects missing installation |
| Final replay of saved gap | 39,670 | 16.573 | reviewer write blocked; goal unverified |
| Final replay with regex proof | 260,392 | 107.966 | repair ingot cracks on generated proof |
| Actual-installation round 1 | 13,935 | 13.741 | named gap: missing installed environment |
| Actual-installation resume | 886,863 | 127.212 tape span | repair ingots attempted; budget cap; goal unverified |
| Minimal fixture review | 6,458 | 3.730 | attempted reviewer install blocked; goal unverified |

The mock test verifies the complete repair order and a successful second verdict.
The live tests prove reviewer confinement and honest failure, but the final PEP fixtures do not produce a repaired installation.
Treat that live repair result as incomplete. The generated proof and invented package path remain separate failure risks.

Total estimated live spend: **$0.34889523**, including the failed attempts and the capped run.
The final successful wc/CSV runs together cost $0.0078208. The JSON contains all measured attempts, including earlier successful runs.
