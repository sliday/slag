# Commission efficiency results

Two live commissions reached FORGED with independent warden passes. Both generated artifacts pass `python3 -m unittest`.

The final build passes the full Rust test suite. Its live rerun remains blocked by Cerebras HTTP 402, "credit balance low."

The successful runs preceded the final retry and recovery-log hardening. The final build has no successful live measurement.

All live requests used `cerebras::qwen-3.8-27b`, the same model as the supplied baseline.

## Measured runs

Wall time covers process start through process exit. Session times use tape timestamps from `context_gauge` through `finish`.

I summed each engine usage event once. Engine totals match the tape totals. Costs use the recorded list-price estimates.

### Exact word-count commission

Commission: `write wc.py that counts words in a file, with a unittest`

Result: FORGED, warden pass, exit 0. Total: **25,978 tokens**, **6.504 seconds**, **$0.0270707 estimated**.

| Phase / session | ID | Role | Prompt | Completion | Seconds |
|---|---|---|---:|---:|---:|
| Scope check | 20261017_105116_300155000 | surveyor | 181 | 669 | 0.879 |
| i1 heat 1 | 20261017_105117_245914000 | smith | 11,136 | 1,408 | 2.556 |
| Warden: artifact | 20261017_105119_952820000 | warden | 11,956 | 628 | 2.591 |
| Role subtotal | n/a | surveyor | 181 | 669 | 0.879 |
| Role subtotal | n/a | founder | 0 | 0 | 0.000 |
| Role subtotal | n/a | smith | 11,136 | 1,408 | 2.556 |
| Role subtotal | n/a | warden | 11,956 | 628 | 2.591 |
| Blueprint / plan negotiation: skipped | n/a | n/a | 0 | 0 | 0 |
| Duel casts and judge: absent | n/a | smith/judge | 0 | 0 | 0 |
| Re-smelt: absent | n/a | founder | 0 | 0 | 0 |
| ASSAY: local | n/a | n/a | 0 | 0 | Untimed |
| Local overhead, including startup and gaps | n/a | local | 0 | 0 | 0.478 |
| **Run total** | n/a | **all** | **23,273** | **2,705** | **6.504** |

Provider calls: 8. One ingot, one heat.

Independent checks: six unittest tests pass. The CLI returns 4 for mixed Unicode whitespace and 0 for an empty file.

### Four-file CSV commission

Commission: `Write a small Python CSV summary utility in four files: csvstats.py with summarize(path) returning the row count and sum of the amount column; cli.py that prints the row count and total for a CSV path; test_csvstats.py with unittest coverage; README.md with usage. Use only the standard library. Ignore blank amount cells. Reject invalid numeric amounts with a clear error and nonzero CLI exit. Test normal input, blank cells, and invalid amounts.`

Result: FORGED, warden pass, exit 0. Total: **69,162 tokens**, **20.310 seconds**, **$0.0746759 estimated**.

| Phase / session | ID | Role | Prompt | Completion | Seconds |
|---|---|---|---:|---:|---:|
| Scope check | 20261017_105154_126315000 | surveyor | 259 | 985 | 3.063 |
| i1 heat 1 | 20261017_105157_251354000 | smith | 34,729 | 9,203 | 12.354 |
| Warden: artifact | 20261017_105209_813525000 | warden | 21,763 | 2,223 | 4.285 |
| Role subtotal | n/a | surveyor | 259 | 985 | 3.063 |
| Role subtotal | n/a | founder | 0 | 0 | 0.000 |
| Role subtotal | n/a | smith | 34,729 | 9,203 | 12.354 |
| Role subtotal | n/a | warden | 21,763 | 2,223 | 4.285 |
| Blueprint / plan negotiation: skipped | n/a | n/a | 0 | 0 | 0 |
| Duel casts and judge: absent | n/a | smith/judge | 0 | 0 | 0 |
| Re-smelt: absent | n/a | founder | 0 | 0 | 0 |
| ASSAY: local | n/a | n/a | 0 | 0 | Untimed |
| Local overhead, including startup and gaps | n/a | local | 0 | 0 | 0.608 |
| **Run total** | n/a | **all** | **56,751** | **12,411** | **20.310** |

Provider calls: 13. One ingot, one heat.

Independent checks: 11 unittest tests pass. The CLI prints three rows and total 4.0 for a blank-cell sample.

The CLI rejects `oops` with exit 1 and a line-specific error. The four product files are `csvstats.py`, `cli.py`, `test_csvstats.py`, and `README.md`.

Both generated git trees remain clean after verification.

## Context measurement

Forge prompts contain 1,351 characters for wc and 2,560 for CSV. The supplied baseline reports 28,000 to 33,000 characters.

The agent retains the original task, two complete tool rounds, and a 4,000-character evidence summary. Its default context budget is 12,000 tokens.

The CSV smith transcript contains four compaction boundaries across seven requests.

I reconstructed message text and tool arguments from the archived transcript. Full-history replay totals 96,701 characters; sent messages total 78,000.

That removes 18,701 characters, or 19.3%, from this replay count. This comparison excludes tool schemas and provider tokenization.

The wc smith needs three requests and no compaction. Its savings come from the shorter task path and prompts.

## Changes and reasons

- Short, fresh utility commissions receive one cheap scope check. The scope excludes external services, UI, deployment, and architectural research.

- Eligible work uses one ingot and one final warden. The coordinator skips blueprint negotiation, founder negotiation, and duplicate ingot review.

- Bars derive from the original requirements. Review prompts permit local imports and omit invented naming and test-count constraints.

- Wardens retain inspection tools and six tool rounds. They lack write, edit, finish, and MCP tools.

- Artifact reviews require a successful source read or shell inspection. Passes require concrete evidence.

- The coordinator commits after status and ledger updates. It commits the final verdict after the warden records it.

- Tool calls and proof commands protect coordinator documents. Recovery snapshots repair indirect log changes and retain active log inodes.

- Resumed strikes use cumulative heat. Duel fallbacks retain their remaining attempts. Final failures preserve the artifact.

## Final verification and live blocker

Final `cargo test` exits 0: 1,019 passed and one ignored. The tests cover rollback, log recovery, honest rejection, and resumed strikes.

`cargo clippy --all-targets` exits 0 with existing repository warnings. `git diff --check` passes.

The final-build live attempt exits 1 after 0.777 seconds. Cerebras rejects the scope request with HTTP 402 before it returns usage.

I stopped live calls after that response. I did not retry or switch providers. The final second commission did not run.

Total recorded live cost estimate: **$0.1017466**, below $1.50. Each live process used a $0.30 run cap and a $0.25 ingot cap.

The host rejects nested `sandbox-exec`; slag reports its fallback. These runs retain the host sandbox.

The recovery guard preserves append-only records. It cannot authenticate the writer of runtime-shaped records. Concurrent writes during repair remain a limitation.

## Exact verification commands

```sh
cd /Users/stas/Playground/slag/.claude/worktrees/lane-efficiency/slag-rs
CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test
CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo clippy --all-targets
cd ..
git diff --check
```

Each commission ran in a fresh directory. The measurement wrapper cleared inherited `SLAG_*` overrides and set these values:

```sh
export SLAG_CONFIG_DIR="<separate empty config directory>"
export SLAG_MAX_COST_RUN=0.30
export SLAG_MAX_COST_INGOT=0.25
export PYTHONDONTWRITEBYTECODE=1
export NO_COLOR=1
```

The key remained in the environment. The wrapper did not print it.

```sh
/Users/stas/Playground/slag/.claude/worktrees/lane-efficiency/slag-rs/target/debug/slag "write wc.py that counts words in a file, with a unittest" --temper 1 --provider cerebras
```

```sh
/Users/stas/Playground/slag/.claude/worktrees/lane-efficiency/slag-rs/target/debug/slag "Write a small Python CSV summary utility in four files: csvstats.py with summarize(path) returning the row count and sum of the amount column; cli.py that prints the row count and total for a CSV path; test_csvstats.py with unittest coverage; README.md with usage. Use only the standard library. Ignore blank amount cells. Reject invalid numeric amounts with a clear error and nonzero CLI exit. Test normal input, blank cells, and invalid amounts." --temper 1 --provider cerebras
```

Artifact verification:

```sh
cd /var/folders/kq/0_xklq7j45q3bbw9ky25yt5h0000gn/T/slag-efficiency-20261017-zu1sxnpa/wc
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest
git status --porcelain
cd /var/folders/kq/0_xklq7j45q3bbw9ky25yt5h0000gn/T/slag-efficiency-20261017-zu1sxnpa/csvstats
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest
git status --porcelain
```

## Evidence and files

[Structured measurements](efficiency-results.json) contain the usage totals, session IDs, verification output, and evidence paths.

Raw logs and artifacts: `/var/folders/kq/0_xklq7j45q3bbw9ky25yt5h0000gn/T/slag-efficiency-20261017-zu1sxnpa`. The wc scratch directory moved there after its measured run.

Changed files:

- `slag-rs/src/engine/agent.rs`
- `slag-rs/src/engine/compact.rs`
- `slag-rs/src/engine/prompt.rs`
- `slag-rs/src/flux.rs`
- `slag-rs/src/pipeline/forge.rs`
- `slag-rs/src/pipeline/mod.rs`
- `slag-rs/src/pipeline/surveyor.rs`
- `slag-rs/src/pipeline/warden.rs`
- `slag-rs/src/smith/mod.rs`
- `slag-rs/src/smith/native.rs`
- `slag-rs/tests/commission_efficiency.rs`
- `slag-rs/tests/forge_loop.rs`
- `notes/gauntlet/cost-efficiency-results.md`
- `notes/gauntlet/efficiency-results.json`

I left the supplied cost analysis unchanged. I did not change the protected modules or git command construction.
