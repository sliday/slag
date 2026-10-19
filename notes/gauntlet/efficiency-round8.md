# Efficiency, round 8

Contracts now put the named behavior and boundary assertions before interface conveniences. Builder, founder, and warden prompts share that priority.

The examples apply to the named operation. They cover wc flags and streaming, CSV boundaries, NFC word counts, and tail selection and follow behavior. Contracts no longer require invented input aliases or a top-N flag. Source launches replace local virtual environment setup.

Read-only commands receive private TMPDIR, TMP, and TEMP paths. The sandbox removes scratch space after each command. Workspace writes remain blocked, including symlink escapes. Python probes run from temporary scripts, including grouped proof commands. The bootstrap supports Python versions without sys.flags.safe_path.

Package installation uses a separate temporary clone and runtime. The strict sandbox permits writes only inside that installation root. It ignores configured writable paths and general temporary-directory grants. A regression backend attempts an absolute write into the original repository and receives PermissionError. The warden must run the installed console check and cite its successful tool ID.

Forge commits use the original commission sentence. Retry notes stay out of commit subjects.

All live calls used OpenAI gpt-6.1-sol for both work and planning. Each run used --temper 1. Costs below come from the engine usage events and remain estimates. The first runs overlapped in time.

| Commission | Seconds | Tokens | Estimated USD | Verdict |
|---|---:|---:|---:|---|
| wc | 139.4 | 118,874 | $0.159039 | FORGED, exit 0 |
| csv | 141.4 | 125,529 | $0.149964 | FORGED, exit 0 |
| node | 113.7 | 105,544 | $0.129390 | FORGED, exit 0 |
| tail-final | 148.5 | 121,301 | $0.171333 | FORGED, exit 0 |

The first tail run returned UNVERIFIED after its checks passed. It cost 136,085 tokens, 177.5 seconds, and $0.190811.

The review kept only two tool rounds and lost its earlier source inspection. It repeated reads and checks, then exhausted its turn budget. Reviews now retain their six bounded rounds. The tail rerun needed three model calls for review, instead of seven. Its review used 17,586 tokens, instead of 44,954. The verdict still requires successful inspection IDs and all required runs.

Total live spend, including that first tail run: **$0.8005367**. No other live provider calls ran.

| Run | Phase | Input tokens | Output tokens | Total tokens | Calls |
|---|---|---:|---:|---:|---:|
| wc | surveyor | 1,268 | 369 | 1,637 | 2 |
| wc | smith | 71,088 | 4,070 | 75,158 | 12 |
| wc | warden | 39,639 | 2,440 | 42,079 | 6 |
| csv | surveyor | 834 | 451 | 1,285 | 1 |
| csv | smith | 85,737 | 3,327 | 89,064 | 17 |
| csv | warden | 34,333 | 847 | 35,180 | 7 |
| node | surveyor | 835 | 443 | 1,278 | 1 |
| node | smith | 84,915 | 3,184 | 88,099 | 17 |
| node | warden | 15,401 | 766 | 16,167 | 3 |
| tail-final | surveyor | 837 | 482 | 1,319 | 1 |
| tail-final | smith | 97,698 | 4,698 | 102,396 | 17 |
| tail-final | warden | 16,871 | 715 | 17,586 | 3 |

Independent checks used committed fresh clones and clean Python runtimes without pip. The suite ran before any project installation.

| Artifact | Fresh-clone suite | Independent checks |
|---|---|---|
| wc | 7 tests, exit 0 | 95 checks. Individual and combined flags match system wc. Empty, unterminated, UTF-8, CRLF, stdin, and dash inputs match. A runtime guard rejects whole-file reads. |
| CSV | 7 tests, exit 0 | 12 checks. BOM and quoted newlines preserve exact values. Short and long rows fail with errors. Module launch, --pretty, stdin, dash, and help pass. |
| Node | 7 tests, exit 0 | 7 checks. Composed and decomposed words share NFC counts. Positional N, stdin, dash, default N, and help pass. |
| tail-final | 11 tests, exit 0 | 12 checks. Last-N selection precedes regex filtering. Zero, +N, invalid patterns, stdin, and help pass. Follow starts at the end with -n 0, follows appends, and buffers partial records. |

The independent CSV installation used another clone and virtual environment. Its installed console handled BOM, a quoted newline, --pretty, and --help outside the source tree.

All four product repositories have clean git status. Their tracked files contain no coordination files, virtual environments, or egg-info. Their committed ignore files contain no slag-specific ignore lines. Forge history uses the commission sentence. The final audit tightened installation confinement and Python compatibility after the live runs; native regressions verify those changes.

Evidence root: `/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r8/`.

- `wc.json`, `csv.json`, `node.json`, `tail.json`, and `tail-final.json`: full usage totals and model calls.
- `independent-verified/{wc,csv,node,tail}.json`: first independent checks.
- `independent-final/tail.json`: checks of the successful tail rerun.
- `independent-verified/csv-install/evidence.json`: independent installed-console checks.
- `repository-checks.json`: tracked files, ignore files, status, and history.
- Each product's `.slag/logs/engine-*.jsonl`: its own warden inspections and verdict.

The exact live commands used these prompts:

```sh
slag "write wc.py that counts lines, words and bytes of a file like wc, with a unittest suite" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a small Python CLI that converts CSV to JSON with a --pretty flag, in a package with tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a tiny Node CLI that prints the top N most frequent words of a text file, with node:test tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a Python CLI that tails a log file and prints only lines matching a regex, with a --follow flag and tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
```

Each driver used the worktree's `slag-rs/target/debug/slag` binary in a fresh project directory. Run caps were $0.32, with a $0.30 cap for the tail rerun.

Final verification: **1,233 passed, one ignored, zero failed**. Cargo returned exit 0. Clippy returned exit 0 with warnings denied.

```sh
export TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r8/tmp
export CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
cargo test --offline --manifest-path slag-rs/Cargo.toml
cargo clippy --offline --manifest-path slag-rs/Cargo.toml --all-targets -- -D warnings
cargo build --offline --manifest-path slag-rs/Cargo.toml
python3 slag-rs/target/round8-independent.py wc
python3 slag-rs/target/round8-independent.py csv
python3 slag-rs/target/round8-independent.py node
R8_CHECK_DIR=independent-final R8_SOURCE_SUFFIX=-final python3 slag-rs/target/round8-independent.py tail
python3 slag-rs/target/round8-install-independent.py
git diff --check
git -C /Users/stas/Playground/slag status --short
```

The independent drivers preserve evidence and refuse to overwrite their existing clone directories.

New regression names:

- `scope_prioritizes_semantics_and_limits_convenience_interfaces`
- `package_probes_use_source_without_a_workspace_install`
- `builder_and_warden_prioritize_semantics_and_isolate_installs`
- `readonly_sandbox_allows_private_temp_and_cleans_it`
- `readonly_temp_symlink_cannot_allow_workspace_writes`
- `readonly_review_shell_supports_tempfile_without_workspace_residue`
- `bounded_review_retains_source_through_required_checks`
- `python_probe_keeps_escaped_quotes_argv_and_workspace_imports`
- `python_probe_supports_flags_without_safe_path`
- `python_probe_preserves_future_imports`
- `python_probe_preserves_isolated_mode`
- `python_probe_materializes_grouped_proofs`
- `python_probe_does_not_reinterpret_shell_operations`
- `commit_subject_uses_commission_before_retry_notes`
- `isolated_install_checks_console_without_workspace_residue`
- `fresh_checkout_suite_can_use_private_tempfile`

Changed files:

- `slag-rs/src/pipeline/direct.rs`: scope priorities and source probes.
- `slag-rs/src/flux.rs`: shared delivery and review expectations.
- `slag-rs/src/engine/sandbox.rs`: private scratch storage and strict installation sandbox.
- `slag-rs/src/engine/agent.rs`: materialized review probes and regression tests.
- `slag-rs/src/engine/probe.rs`: safe Python probe materialization.
- `slag-rs/src/engine/mod.rs`: probe module declaration.
- `slag-rs/src/pipeline/forge.rs`: proof execution and user-facing commit subjects.
- `slag-rs/src/pipeline/warden.rs`: required isolated installation evidence.
- `slag-rs/src/pipeline/warden/checkout.rs`: isolated installation and fresh-clone tests.
- `slag-rs/src/smith/native.rs`: bounded source retention and scratch policy text.
- `README.md` and `CLAUDE.md`: sandbox and verification behavior.
- `notes/gauntlet/efficiency-round8.md`: this report.
