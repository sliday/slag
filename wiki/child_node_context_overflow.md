---
name: child_node_context_overflow
desc: Failure pattern — child dies every step on `agent error (exit 1)`; two causes (context overflow, exhausted model credits) told apart by accrued cost.
tags: [fractal-ops, troubleshooting]
created: 2026-08-26T02:37:57Z
updated: 2026-08-26T09:45:00Z
---

# child_node_context_overflow

## Symptom

A child node ships nothing: every step — even a seconds-long SYNC —
ends `agent error (exit 1)` in `fractal node activity <branch>`, the
child's plans dir stays empty, and its branch holds only seed commits.
`claude.err` in the node dir is typically empty; do not stop there.

Two different faults produce this one signature. **Read the cost column
first — it tells them apart:**

| Accrued cost | Cause |
|---|---|
| Cost per attempt | Context overflow (below) |
| Exactly `0.0` | Model credits exhausted ([[#exhausted-model-credits]]) |

## Root cause: context overflow

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

## Exhausted model credits

A child whose model has no usage credits left fails identically —
uniform `agent error (exit 1)`, empty `claude.err`, empty plans dir —
but bills **exactly `0.0`**, because no request is ever accepted. A
whole run can burn its full iteration allowance in about two minutes:
PREPARE fails, then PLAN / EXECUTE / REVIEW / COMMIT each end
`failed on PREPARE`, and the run exits "Reached max iterations".

The node's `config.json` is a red herring here — model string, scope,
and `detached` all read correct, because the config is correct. The
credit pool behind the model is what is empty.

### Diagnosis

Probe the model string directly, outside the harness:

```
claude --model '<model-from-config.json>' -p 'say OK'
```

"You're out of usage credits" names the fault outright. Note the CLI
prints that message and still **exits 0**, so branch on the text, not
the exit code.

### Remedy

Repoint the child at a funded model of the same context class, then
relaunch:

```
fractal node config set "model=<funded-model>"
fractal node start --continue
```

`--continue` grants fresh iterations, and a run that ended on max
iterations (not on budget) accepts it bare — no `--max-cost` needed.
Confirm the fix by watching `fractal node activity`: a step that stays
active well past the ~4s failure point is really working.

Keep the replacement in the same context class. Substituting a
200k-window model to dodge a credit wall trades this failure for the
context overflow above.

### Prevention

Before spawning on any model, check what funded children actually ran:

```
for n in <sibling-branches>; do
  python3 -c "import json;print(json.load(open('$n/.fractal/$n/config.json'))['model'])"
done
```

A model string that no completed node has ever used is worth one probe
before it costs a spawn ceremony.
