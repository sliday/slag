---
name: bash_guard_and_analytics_surfaces
desc: Where slag's bash guard rails, background bash, and offline analytics live — orientation map for nodes touching engine/tools.rs or the logs heap.
created: 2026-08-26T02:53:38Z
updated: 2026-08-26T02:53:38Z
---

# bash_guard_and_analytics_surfaces

Orientation for nodes working on slag's tool layer or log analytics.
Authoritative detail lives in `notes/inspiration-100.md` `_shipped:`
lines (items 80, 95, 96, 97, 100); this page maps the code.

## Bash guard stack (slag-rs/src/engine/tools.rs, in check order)

1. Destructive-command gate (`destructive_warning`) — env override
   `SLAG_ALLOW_DESTRUCTIVE=1`.
2. Policy engine (`engine/policy.rs`, `[policy]` config table) —
   deny > ask > allow; attached via `ToolBox::with_policy`.
3. Background branch: `run_in_background=true` skips the sleep guard,
   runs `sh -lc` in a detached process group (no timeout, no
   kill_on_drop), streams stdout+stderr to `logs/bg/<id>.log`, and a
   waiter task pushes a completion note into the agent's `SteerQueue`
   (`ToolBox::with_steer`; duel casts stay steerless).
4. Sleep guard (`blocked_sleep`) — leading integer `sleep N`, N>=2,
   refused with remedies (raise timeout / run_in_background / until).

Read-only classification for concurrent scheduling:
`read_only_bash()` + the `path_access` bash arm (fail-closed on
substitution, redirects, wrappers, unknown commands).

## Log heap and analytics

- `logs/run-*.jsonl` — per-run ledger (`RunEntry`: run_meta,
  ingot_done, note, assay). Serialize + Deserialize.
- `logs/<other>-*.jsonl` — engine event firehose (tag `event`:
  tokens/tool_result/duel_verdict/…).
- `logs/transcripts/` — per-ingot resume transcripts.
- `logs/bg/` — background bash output.
- `logs/facets/<stem>.json` — `slag insights` cache: schema-stamped,
  reused while at least as new as its log; `--refresh` recomputes.
- All JSONL parsing goes through
  `engine/transcript.rs read_jsonl_tolerant` (skip bad lines, drop
  truncated tail) — reuse it for any new logs reader.

`slag insights [--refresh]` (src/insights.rs) prints forged/cracked,
heats, spend, tokens, per-tool error counts, duel margins. Offline, no
key.

## Path-error hints (tools.rs)

Failed path lookups self-correct in one turn: `resolve()` escape errors
and `read_file` cannot-read errors append `path_hint()` — the workspace
root plus up to three same-basename matches from a bounded walk (~4000
entries, skips `.git`/`target`/`node_modules`/`.venv` and symlinked
dirs). The `edit_file` spec also carries the minimal-uniqueness hint
(smallest clearly-unique old_string, 2-4 lines; add context or
replace_all on non-unique matches).
