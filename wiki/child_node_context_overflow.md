---
name: child_node_context_overflow
desc: Failure pattern — continued child dies every step on "Prompt is too long"; diagnosis and remedy.
tags: [fractal-ops, troubleshooting]
created: 2026-08-26T02:37:57Z
updated: 2026-08-26T02:37:57Z
---

# child_node_context_overflow

## Symptom

A child node burns budget but ships nothing: every step — even a
seconds-long SYNC — ends `agent error (exit 1)` in
`fractal node activity <branch>`, cost accrues per attempt, the child's
plans dir stays empty, and its branch holds only seed commits.

## Root cause

The agent's prompt exceeds the model context window. Each step start
bills input tokens, the API rejects with "Prompt is too long", the CLI
exits 1. A common trigger: continuing (`--continue`) a node whose prior
session transcript was already bloated — the restored session re-hits
the limit on every subsequent step, forever.

## Diagnosis

1. `fractal node activity <branch>` — uniform `agent error (exit 1)`
   across step types is the signature (content-level failures vary by
   step; context overflow does not).
2. Newest transcript in `~/.claude/projects/<worktree-slug>/*.jsonl`:
   grep `isApiErrorMessage` and read the last assistant text — it says
   "Prompt is too long".
3. `claude.err` in the node dir is typically empty; do not stop there.

## Remedy

- Kill the child; do not raise its cap or `--continue` it — the restored
  session guarantees a repeat.
- If its branch has only seed commits, skip the merge and re-assign the
  work (fresh child with a fresh session, or absorb in the parent).
- Watch for it early: a child whose first SYNC fails with exit 1 in
  ~10s deserves an immediate transcript check, before iterations pile up.
