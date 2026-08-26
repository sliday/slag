---
name: inspiration_100_backlog_audit
desc: The inspiration-100 backlog is closed; how its checkboxes drifted from the tree while it was open, which modules the CLAUDE.md layout table still omits, how a _shipped: note goes wrong in both directions, and the shared conventions for state under ~/.slag/.
created: 2026-08-26T07:35:00Z
updated: 2026-08-26T13:20:00Z
---

# inspiration_100_backlog_audit

`notes/inspiration-100.md` is a backlog file, not a build artifact. That
backlog is now closed — every item is `[x]` (see **Backlog state:
closed** below). What follows is the audit that closed it, kept because
the failure modes outlive this file: they belong to any checkbox list
that tracks a moving tree.

While it was open, its checkboxes lagged the source tree by whole waves,
in two ways:

1. A wave ships an item and never flips the box.
2. A **sibling** wave ships an item on `main`; the merge brings the code
   in, but the notes file keeps its `[ ]`.

Both bit nodes here. Grep the code before implementing any item.

## Modules the layout table omits

Commit `75a3260 release(2.5.0): wave 4 — sessions, transcripts, process
surface` arrived through a `main` merge and added two modules absent
from the `CLAUDE.md` source-layout table:

| Module | Public surface | Covers |
|--------|----------------|--------|
| `slag-rs/src/progress.rs` | `spinner_status`, `LiveStatus`, `token_rate`, `forge_title`, `progress_state`, `report_forge_state`, `clear_forge_state` | live status line, terminal title, OSC progress |
| `slag-rs/src/anvil/checkpoint.rs` | `Checkpoint::{begin,record,rewind}`, `rewind_attempt`, `rewind_latest`, `BackupEntry` | pre-edit file checkpoints and failed-heat rewind |

Two more modules were likewise absent from the table, and already
satisfied items the notes still carried as open at the time:

| Module | Public surface | Covers |
|--------|----------------|--------|
| `slag-rs/src/engine/transcript.rs` | `TranscriptWriter`, `is_resumable`, `resumable_messages`, `resume_attempts`, `mark_resume` | per-ingot JSONL transcripts, mid-ingot resume |
| `slag-rs/src/migrations.rs` | `run`, `rewrite_model_slugs`, `upgrade_crucible_header` | idempotent startup migrations |

`RunEntry` lives in `engine/events.rs` and is consumed by `insights.rs`
and `pipeline/forge.rs`. `slag ps` wiring is present in `cli.rs` and
`main.rs`.

`slag-rs/src/steer_history.rs` (`record`, `flush`, `recall`,
`install_flush`, `history_path`) is also missing from that table — it
landed after it was written. It owns `~/.slag/history.jsonl`.

## The pattern for anything under `~/.slag/`

Three modules now write process-shared state there (`sessions/<pid>.json`
from item 83, `history.jsonl` from item 85), and they converged on the
same three moves. Copy them rather than re-deriving:

- **Env-first path.** `$SLAG_HISTORY_FILE` / `$SLAG_SESSIONS_DIR`
  overrides, else `$HOME/.slag/…`. Without the override the tests write
  into the developer's real home directory.
- **Exclusion via `OpenOptions::create_new`** on a `.lock` sibling. The
  OS picks the winner, so two forges racing cannot both believe they
  won. Bound the retries, and break a lock past a staleness threshold —
  a crashed process otherwise wedges the file forever. Advisory `flock`
  is the wrong tool here: the writers are separate processes with no
  shared fd.
- **Register the write with `shutdown::register`**, do not call it at the
  end of the happy path. The exit worth protecting is the one that never
  reaches the end of the function. `shutdown.rs` runs cleanups
  reverse-order under `catch_unwind` from both the Ctrl-C handler and the
  panic hook.

Corollary for the keypress path: buffer in memory and flush on shutdown.
A dashboard that takes a cross-process file lock while the operator is
mid-sentence stalls the typing, and the stall is invisible in tests.

## Auditing cheaply

Sweep candidate symbols across `src/` before scoping work:

```sh
cd slag-rs
for p in RunEntry "PidRegistry|slag ps" "checkpoint|rewind" "OSC|9;4"; do
  printf '%-28s' "$p"; rg -il "$p" src/ | tr '\n' ' '; echo
done
```

**ripgrep gotcha that produced a false all-clear here:** rg reads Rust
regex syntax, so `|` is alternation and `\|` is a *literal pipe*. A
pattern written `"a\|b"` searches for the six-character string `a|b`
and matches nothing. Write `"a|b"`.

Presence of a symbol is evidence, not proof: read the function and
check the item's `_evidence:` line before flipping a box. Where an item
is genuinely already satisfied, flip it to `[x]` with a `_shipped:`
note naming the commit or module rather than re-implementing it.

## The `_shipped:` note is a claim of record, so verify it separately

Flipping the box and writing the note are two claims, and the second
fails on its own. Four of twenty notes written here were wrong or thin
when re-checked against the source in the same session:

| Item | Note said | Source said |
|------|-----------|-------------|
| 52 | `compact.rs overflow_gap_chars()` | `agent.rs shrunk_output_cap()`; compact.rs parses the same 400 body for a different item (45) |
| 48 | client-side switch on capacity errors | OpenRouter's native `models: [primary, fallback]` array, one round trip |
| 49 | backoff capped at 5 minutes | that, plus the 30s heartbeat slices and the `Retry-After: 0` floor, which are the item's distinctive half |
| 20 | `output_mode`, context, `head_limit` | those plus `-i` and a `glob` filter |

The failure mode behind item 52 is the dangerous one: **a
same-shaped symbol in a neighboring module reads like a hit.** Two
modules parsing the same provider 400 body serve two different items.
Grepping the mechanism ("parse the overflow numbers") finds both;
only the call site tells you which item each satisfies.

Two rules that catch all four:

- Grep for the symbol **the item's own spec names**, then confirm the
  call site does what the item asks. Item 52 asks for a retry with a
  shrunk `max_tokens`; only `agent.rs` retries.
- Read the whole spec line before writing the note. Under-claiming
  ("capped at 5 minutes") sends the next reader to re-implement the
  half you already have.

Cheapest guard: name the covering test in the note. A note that cannot
name one is a note about code you have not read.

## A spec with two halves needs both halves checked

The rules above catch a note pointing at the wrong module. They miss the
opposite failure, and it is worse: a note that points at the **right**
module for **half** the spec.

Item 82 reads "`slag runs` subcommand **+ dashboard run picker**". The
subcommand is real and complete. The picker does not exist —
`rg 'run_picker|list_runs' src/dashboard.rs` is empty. A grep for the
item's headline symbol (`slag runs`) hits, the call site checks out, and
the box flips over a feature that is half-built.

Under-claiming wastes a reader's time; over-claiming means **nobody ever
builds the missing half**, because the box says it is done. The flipped
box is the only record anyone consults later.

Guard: count the conjunctions in the spec line. An item joined by `+`,
`and`, or a semicolon is two claims, and each needs its own grep. Where
one half is genuinely missing, flip with a `**Partial**` clause naming
the gap rather than a clean `_shipped:` — item 82 carries one, and says
why the picker was left (three writers already held `dashboard.rs`; a
fourth buys a merge conflict, not a feature).

## When a note honestly cannot name a test

The name-the-test rule has one legitimate exception, and pretending
otherwise invites an invented test name. Item 84's mechanism is an
ordering guarantee — `resume_hint()` must print *after* the ratatui
teardown — living in `main.rs`, which carries no test module. No unit
test can observe it.

Write that instead of a name: say which call site was read and what the
guarantee is. An honest "no test, here is why, here is the line I read"
is verifiable. A plausible test name that does not exist is not.

## Audit the shipped code against its own doc comment

Every rule above checks a `_shipped:` note against the tree. One more
check runs the other way: read the shipped function's doc comment as a
list of claims, and test each clause.

Three defects in the session/transcript/run-ledger cluster survived a
full build-and-test pass and fell to this check alone. All three were a
comment promising something the code did not do:

| Claim in the comment | What the code did |
|---|---|
| `flush` — "giving up loses nothing" | Re-buffered on lock contention, dropped the tail of a batch that died mid-write |
| `rewind` — "no checkpoint to rewind" (bare arm) | The named-target arm printed "rewound i9-h1: 0 file(s) restored" for an ingot never checkpointed |
| `rewind_latest` — "Unused until the CLI group wires it" | `cli.rs` wires it |

The species is the same each time: a contract stated once, then
implemented across two code paths written at different moments. The
tested path works. Its neighbour does not.

Cheap and mechanical: count the clauses in the doc comment, then find
the assertion for each. A clause with no test is either an untested path
or a false claim, and reading the code tells you which in a minute.

## `#[allow(dead_code)]` is a claim with an expiry date

`rewind_latest` carried an allow and a comment saying the CLI had not
wired it yet. Both outlived the wiring. Nothing failed, because an
unnecessary allow is silent by construction — that is the whole problem
with it as a record.

When a change wires a previously-unused function, deleting its allow
belongs to that change. The compiler then re-checks the claim on every
build, which no comment does. A leftover allow also masks the *next*
regression: unwire the caller later and the function goes quietly dead
again.

## Backlog state: closed

Every item in `notes/inspiration-100.md` is `[x]` with a `_shipped:` note
naming the symbols and the tests guarding them. The file is the record;
read the item line before touching any of that code.

Four `_shipped:` notes record a **clause with no site** — a spec sentence
that named a file which turned out not to have the surface the spec
assumed. These are verified absences, not skipped work, and re-opening
them would re-buy the same investigation:

| Item | Clause | Why there is no site |
|------|--------|----------------------|
| 50 | "recipe suggestions" get a side retry policy | `engine/recipes.rs` constructs no `ChatRequest` |
| 55 | rebuild the vec on the fallback-model path | slag has no fallback-model path; the analogues are escalation, nudge, and overflow-shrink |
| 55 | mirror in `duel.rs` round retries | each duel round builds fresh worktrees, prompts, and smiths, so no message vec crosses a round |
| 60 | age `PROGRESS.md` history | it is injected as a 25-line tail of an append-only ledger, so the file mtime says nothing about the lines shown |

## Two traps in the prompt-assembly path

Both cost real debugging on the last wave; both are invisible until the
wrong fix is already written.

**A cached string cannot hold a computed age.** `engine/recipes.rs`
caches its rendered index against a manifest of recipe mtimes. Baking a
"written N days ago" into that index freezes the number on the day the
cache was written — the file has not changed, so the cache stays valid
while the sentence rots. Stamp file age where the file is *injected*
(`recipe_view`), never where it is *indexed*.

**The stable band must stay byte-stable.** `stable_band_is_byte_stable_across_runs`
(`engine/prompt.rs`) is the guard. Per-run data — file paths, sizes, ages
— belongs in flux or the volatile band. Only a constant contract sentence
may enter the stable band.

## `run_shell` is the one shell choke point

`bash`, `grep`, and `glob` all route through `ToolBox::run_shell`, so a
change to output handling or process control lands once and covers three
tools. The exception worth knowing: `grep` applies its own `head_limit`
to lines already in memory *after* `run_shell` returned, so a cut made
there is invisible to anything `run_shell` does and needs its own
handling.

`process_group(0)` on the spawned command is what makes `kill -9 -<pid>`
reach the whole tree, so both the timeout path and the interrupt path
kill a shell pipeline rather than orphaning its children. Tokio's `Child`
does **not** kill on drop, so any `select!` arm that abandons the wait
future has to send that signal itself.

### Two layers that both cut must reconcile, not stack

Giving each cut its own overflow spill looked right and shipped a false
claim. A grep past both the byte cap and the line limit wrote two files:
`run_shell` spilled the true pre-cap output, then the tail cut spilled
the *already-truncated* remains and labelled them "full N matches". The
second file is strictly smaller than the first and the note calling it
complete sends the reader to the wrong one.

Two rules for any layered truncation:

- **The outer cut owns the artifact.** An inner layer that finds the
  outer one already spilled names that file instead of writing its own.
- **A layer's own annotations are not data.** `run_shell` appends its
  spill note to the text `grep` then splits into "match lines", so the
  note counted as a match and padded the `(truncated: N more lines)`
  arithmetic. Filter a preceding layer's notes out before counting.
- **Recognise your own note by its opening, not by a path it mentions.**
  The first filter keyed on `logs/tool-results` and would have discarded
  a genuine match quoting that path — which source in this repo does. A
  distinctive prefix constant shared by writer and reader is the fix; a
  substring that user data may also contain is not.

A test that asserted only "the output names a spill path" passed against
both files and could not see either defect. Assert the *count* of
artifacts and that the named one holds what the note claims.

## A type with tests and no consumer is not the feature

`CancelReason` (`UserAbort` / `SteerInterrupt`) landed with a `label()`,
six unit tests, and zero production readers: nothing branched on it, so
the spec clause it existed for — a steer interrupt skipping the error
path — was still unbuilt while the box read shipped. Unit tests over a
type prove the type, never the wiring.

`grep -rn 'TypeName' src/ | grep -v 'mod.rs\|test'` answers it in one
line. When a capability genuinely ships ahead of its caller, say so in
the note (`ForgeAgent::check_cancel` branches on the reason; no surface
raises `SteerInterrupt` yet) rather than letting the reader infer a live
path from a passing test.

## Test-only imports surface as release warnings, not test failures

`cargo test` compiles `#[cfg(test)]` code, so an import used only by
tests looks used. Move the last production use of it away — replacing a
raw `AtomicBool` load with a wrapper method, say — and `cargo test` stays
green while `cargo build --release` gains an `unused import`. Against a
"no new warnings" gate that is a real regression the test suite cannot
see.

Count warnings on both sides of a change:
`cargo build --release 2>&1 | grep -c '^warning:'`. The standing baseline
here is **22**, all dead-code warnings in files this backlog never
touched. When the count moves, `grep -E 'agent.rs|flux.rs|tools.rs'` over
the same output names the file.

The baseline read 23 through most of this work. The extra one was a stale
`use crate::sexp::{Ingot, Status}` in `pipeline/resmelt.rs`, dead since
`09cebbf` and unrelated to any item here; it was dropped at the final
gate. Anyone comparing against an older note should expect 22.
