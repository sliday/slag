# Efficiency round 7

## Result

All three commissions reached FORGED on their first runs. Each warden ran the declared suite in a clean clone under the read-only sandbox. Independent fresh-clone checks passed without project installation.

| Commission | Tokens | Seconds | Estimated USD | Verdict | Fresh-clone tests |
|---|---:|---:|---:|---|---:|
| wc | 76,300 | 109.517 | $0.1108074 | FORGED | 7 |
| csv | 75,274 | 112.386 | $0.1021420 | FORGED | 7 |
| node | 104,647 | 109.787 | $0.1297082 | FORGED | 11 |

Estimated spend totals **$0.3426576**, below $1.20. These were the only provider runs. No rate or quota error occurred.

Both model roles used `openai::gpt-6.1-sol`. The binary SHA-256 is `74f3fe4b3be022bb9303846358c3f286ab66773dc6b3a9edc2372ff0476ebe84`.

## Changes

- Contracts require a documented suite command that works from a fresh clone without project installation.
- Python package CLIs include `__main__.py`, module launches, and console entry points.
- File CLIs accept no-file stdin and literal `-`. Top-N CLIs accept `FILE N` and the flag form.
- The coordinator clones committed HEAD without local runtime files. It creates a separate Python runtime without pip or installed packages.
- The warden runs the cloned suite itself. Its pass must cite that successful tool result. Missing or failed cloned checks cannot clear the goal gate.
- Slag stores its metadata ignore patterns in local Git excludes. Language cache rules remain in the committed `.gitignore`.
- Bootstrap commits use `chore: initialize project`. Language-neutral requests can use an empty bootstrap commit.
- Legacy resume initializes local excludes before parallel worktrees start. Separate Git metadata directories remain supported.

## Independent checks

I cloned each result into a separate job directory. I ran its documented command before any project installation. Python used separate stdlib runtimes without pip. The sandbox refused filesystem writes.

| Artifact | Documented suite command | Additional checks |
|---|---|---|
| wc | `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest` | File, omitted-file stdin, literal `-`, help, and module launch. Counts match system wc for newline, unterminated, empty, and UTF-8 inputs. |
| CSV | `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest` | `python3 -m csv_to_json`, positional input, `--pretty`, help, omitted-file stdin, and literal `-`. The original installed console also runs. |
| Node | `node --test` | Positional `FILE 2`, flag `FILE --top 2`, both stdin forms, help, default N=10, and the package bin mapping. |

On this macOS host, system `wc -` treats `-` as a filename. The independent count comparison uses system wc without arguments for stdin. The generated CLI accepts `-` as requested.

Independent results, exact argv, stdout, and stderr reside under `independent-verified/` in the job directory. `extra.json` records local excludes and each grounded warden verdict.

## Repository history and ignores

These are the actual `git log --oneline` and `git show HEAD:.gitignore` results from the fresh clones.

### wc history

```text
461f214 forge(i1): write wc.py that counts lines, words and bytes of a file like wc, with a unittest suite
c693aee chore: initialize project
```

### wc committed .gitignore

```gitignore
__pycache__/
*.pyc
*.pyo
.pytest_cache/
.mypy_cache/
.ruff_cache/
.venv/
venv/
.coverage
htmlcov/
build/
dist/
*.egg-info/
```

### csv history

```text
657c6de forge(i1): a small Python CLI that converts CSV to JSON with a --pretty flag, in a package with tests
c693aee chore: initialize project
```

### csv committed .gitignore

```gitignore
__pycache__/
*.pyc
*.pyo
.pytest_cache/
.mypy_cache/
.ruff_cache/
.venv/
venv/
.coverage
htmlcov/
build/
dist/
*.egg-info/
```

### node history

```text
408d4f0 forge(i1): a tiny Node CLI that prints the top N most frequent words of a text file, with node:test tests
0225d24 chore: initialize project
```

### node committed .gitignore

```gitignore
node_modules/
dist/
build/
.next/
.nuxt/
.turbo/
coverage/
```

The original repos keep `/.slag/` and `/logs/` in `.git/info/exclude`. Fresh clones contain no tracked coordination documents. The original repos and the fresh clones have clean Git status.

## Phase tokens

| Run | Phase | Input | Output | Total | Calls |
|---|---|---:|---:|---:|---:|
| wc | surveyor | 760 | 533 | 1,293 | 1 |
| wc | smith | 52,607 | 3,280 | 55,887 | 9 |
| wc | warden | 17,737 | 1,383 | 19,120 | 3 |
| csv | surveyor | 614 | 585 | 1,199 | 1 |
| csv | smith | 54,538 | 3,145 | 57,683 | 10 |
| csv | warden | 15,376 | 1,016 | 16,392 | 3 |
| node | surveyor | 615 | 232 | 847 | 1 |
| node | smith | 84,165 | 2,928 | 87,093 | 17 |
| node | warden | 15,807 | 900 | 16,707 | 3 |

## Regression checks

Cargo returned exit code 0: **1,218 passed, 1 ignored**. Clippy returned exit code 0 with warnings denied.

- `fresh_clone_suite_rejects_an_installed_environment`
- `first_review_conventions_cover_module_dash_and_positional_launches`
- `fresh_checkout_excludes_untracked_files_and_local_runtime`
- `fresh_checkout_refuses_uncommitted_import_dependency`
- `fresh_checkout_prefixed_python_uses_isolated_runtime`
- `fresh_checkout_tests_cannot_write`
- `a_local_only_suite_cannot_ground_a_fresh_clone_pass`
- `coordinator_ignores_stay_local_and_language_rules_remain_portable`
- `language_neutral_commissions_create_only_local_excludes`
- `local_excludes_support_separate_git_metadata`
- `local_excludes_refuse_symlinks_and_external_parents`

The mock-provider regression starts with a suite that passes in the original workspace. Its cloned check fails on a local-only dependency. A model pass that cites only the successful original run becomes UNVERIFIED.

## Exact commands

Rust verification ran from the worktree root:

```sh
TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r7/tmp CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test --offline --manifest-path slag-rs/Cargo.toml
TMPDIR=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r7/tmp CARGO_HOME=/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r3/cargo-home CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo clippy --offline --manifest-path slag-rs/Cargo.toml --all-targets -- -D warnings
git diff --check
git -C /Users/stas/Playground/slag status --short
```

Live commands ran in separate fresh directories:

```sh
slag "write wc.py that counts lines, words and bytes of a file like wc, with a unittest suite" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a small Python CLI that converts CSV to JSON with a --pretty flag, in a package with tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
slag "a tiny Node CLI that prints the top N most frequent words of a text file, with node:test tests" --temper 1 --provider openai --model openai::gpt-6.1-sol --plan-model openai::gpt-6.1-sol
```

The independent clone command used `git clone --quiet --no-hardlinks --no-local SOURCE DEST`. Each suite then ran from DEST under a read-only sandbox.

Evidence root: `/Users/stas/.claude/jobs/f353e9d3/tmp/efficiency-r7`.

## Files changed

- `CLAUDE.md`
- `README.md`
- `notes/gauntlet/efficiency-round7.md`
- `slag-rs/Cargo.toml`
- `slag-rs/src/flux.rs`
- `slag-rs/src/pipeline/direct.rs`
- `slag-rs/src/pipeline/hygiene.rs`
- `slag-rs/src/pipeline/mod.rs`
- `slag-rs/src/pipeline/warden.rs`
- `slag-rs/src/pipeline/warden/checkout.rs`
- `slag-rs/tests/commission_efficiency.rs`

`engine/provider.rs` remains unchanged. The main checkout stays clean. Its local exclude file retains its pre-session modification time.
