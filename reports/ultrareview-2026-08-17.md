# slag v2.0.0 — Ultrareview Audit (2026-08-17)

Scope: 7,619 lines of new code — engine/, pipeline/duel.rs, dashboard.rs, smith/native.rs, worktree changes.
Method: 22 agents. Six hostile finder lenses (engine core, tool sandbox, duel+git, TUI+steering, prompt injection, cost safety) → dedup by file:line → one skeptic per serious finding, instructed to refute with code traces and live repros (uncertain = refuted) → fix agent with regression tests → full-suite temper.
Numbers: 53 raw findings → 49 deduped → 14 serious sent to skeptics → **9 confirmed** (all fixed) → **1 refuted** → 21 minors (8 fixed in the follow-up sweep, 13 documented below). Tests: 174 → 187 green. ~2.1M subagent tokens.

## Confirmed and fixed (9)

1. **agent.rs:25 — fixed 600K char budget, no context-overflow reaction.** Models with 32K–131K context (incl. the default duel alt kimi-k2) overflowed before compaction ever fired; each heat re-billed the full ramp-up until the ingot cracked. Fix: `SLAG_CHAR_BUDGET` override + overflow classifier + shrinking retry — on a context-window 400 the agent halves the budget (floor 16K), compacts, resends; bails when compaction stops progressing.
2. **tools.rs:370/327 — false "already applied" success.** `content.contains(new_string)` ran BEFORE the fuzzy ladder; `contains("")` is always true, so every whitespace-drifted deletion silently no-opped with a success message. Fix: ladder runs first; heuristic only after all strategies fail, never for empty new_string.
3. **tools.rs:442 — bash timeout killed only `sh`, not grandchildren.** `cargo build` timeout left rustc workers running; backgrounded commands held the stdout pipe and stalled the full timeout. Fix: `process_group(0)` + kill the whole group on timeout. Verified with a live orphan repro.
4. **tools.rs:397 — fuzzy strategies spliced drifted indentation verbatim.** A tab-drifted needle against a space-indented Python file wrote tabs into the file (TabError). Fix: line-trimmed and whitespace-normalized strategies now reindent to the file's indentation, like the indentation-flexible strategy always did.
5. **duel.rs:283 — swallowed `git commit` failure → false Merged/Forged.** Fix: staged-but-uncommitted state now surfaces as WorktreeError.
6. **duel.rs:286 — dirty-main merge abort destroyed the proven winner.** Fix: overlap-aware wait before merging; merge-failure paths preserve the winner's worktree and branch.
7. **worktree.rs:27 — leaked worktrees permanently blocked future duels** (deterministic branch names). Fix: `create_in` reclaims (prune, force-remove, branch -D) before adding.
8. **cli.rs:20 — `--worktree` parsed but never read.** Fix (truthful minimum): loud warning + honest help text until the isolation is wired.
9. **agent.rs:97 — steer messages destroyed when the provider errored after drain.** Fix: applied steers are requeued at the front of the shared queue on any error, so the retry heat re-delivers them.

## Refuted (1)

- **agent.rs:105 — "Length-continue path drops reasoning_details on replay."** Skeptic traced the actual replay path; the details are preserved. Not a bug.

## Minors fixed in the follow-up sweep (8)

bash timeout ceiling (600s cap) · ANSI/control-char injection via event previews · duel rounds clamped to remaining heat budget · synthesized tool-call ids unique across turns · git commit subjects sanitized before entering the system prompt · Crucible::load warns on dropped malformed ingot lines · recipes index capped (50 recipes / 200-char descriptions) · SlagError::Cancelled propagates out of duel casts (Ctrl-C aborts duels).

## Documented, not fixed (13) — ordered by my judgment of priority

1. **forge.rs:126** — CRUCIBLE_LOCK held across a full resmelt smith invocation: serializes anvils for minutes. Needs a lock-scope redesign, not a patch.
2. **provider.rs:103** — every send/read error retried, including the 600s client timeout: worst case ~30 min per call chain. Retry policy deserves a deliberate pass.
3. **tools.rs:288 / recipes.rs:67** — TOCTOU between symlink check and use; symlinked RECIPE.md reads outside the workspace. Real but requires openat-style fixes.
4. **compact.rs:54** — operator steers appended to tool results can be pruned away by compaction (steer text lost from context).
5. **tools.rs:405** — fuzzy edits rebuild the file with a single detected line ending: mixed-EOL files get normalized as a side effect.
6. **tools.rs:574** — `find_all` counts non-overlapping occurrences; overlapping-needle uniqueness check can miscount.
7. **provider.rs:118/109** — cost accounting misses billed-but-unparseable responses; network errors during body read conflated with malformed JSON.
8. **duel.rs:232** — assayer critique reuses "cast A/B" labels across position-swapped rounds; casts can misattribute advice.
9. **main.rs:81/90** — `--tui` gates on stdin but renders to stderr; TUI wires up on the ClaudeSmith fallback path where steering does nothing.
10. **dashboard.rs:392** — key-reader stop flag and cleanup only run on the normal return path.
11. **prompt injection (residual)** — workspace content (README, recipes, tool output) enters the prompt unfenced. The sweep sanitized git subjects and previews; a full untrusted-content fencing pass is the real fix and belongs in v2.1.
12. **recipes snapshot mtime granularity** (known from round 2).
13. **duel base pinning + post-merge proof re-run** (known from round 2) — matters only under heavy parallel churn.

## Verdict

The engine core is now solid under adversarial review: every confirmed serious defect had a fix with a regression test the same day. The remaining risk concentrates in three themes for v2.1: retry/lock policy (items 1–2), filesystem trust boundaries (3), and untrusted-content fencing (11).
