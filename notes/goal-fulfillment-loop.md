# Goal fulfillment loop (design)

slag verifies tasks. It never verifies the goal.

`:proof` is mechanical — `test -f`, `npm test`, `grep -q`. It answers "did this
task's artifact appear", never "does the thing we were asked for exist". ASSAY
tallies counts. The finish summary is written by the smith that did the work,
which is the builder grading itself. A run can report `12 forged · 0 cracked ·
tests 24/24 green` for a commission it never came close to fulfilling.

Two missing pieces, per the gauntlet method: a **bar** the work can be measured
against, and a **judge that is not the builder**.

## The insight

slag already owns both recursion mechanisms this needs:

| level | failure loop | already exists |
|---|---|---|
| ingot | re-smelt → SPLIT into sub-ingots | yes, `pipeline/resmelt.rs` |
| commission | addendum → `founder::extend` → new ingots | yes, shipped 2.7.1 |

So this is not new machinery. It is a bar and a warden wired to loops that are
already there.

## Shape

```
commission ─ BAR.md ─────────────── warden judges the artifact
  └─ ingot i1 ─ :bar ────────────── warden judges the ingot
       └─ sub-ingot i1a ─ :bar ──── warden judges the sub-ingot
```

Same check at every depth. That is the fractal part: a node has a goal, a bar, a
judge, and a way to generate its own next issues when it loses.

## The bar

Derived once from the commission by a fresh agent, written to `BAR.md`, read by
every warden. It must be a thing an agent can open and inspect: a checklist item,
a measurement, a named comparison. Not an adjective.

Per-ingot: a new `:bar` field on the s-expression, distinct from `:proof`.
`:proof` stays the cheap mechanical gate that runs every heat; `:bar` is what the
warden reads when it judges whether the sub-goal was actually met.

## The warden

A fresh-context critic. Never the smith, never given the smith's reasoning or its
summary. Gets: the goal at its level, the bar, and tools to inspect the real
artifact — it runs the build, runs the tests, reads the files, screenshots the
page. A warden reasoning from a summary is grading a summary.

Verdict is structured, never prose:

```
VERDICT: pass | fail
GAP:     the single biggest gap that still matters
EVIDENCE: what it actually inspected (file:line, a number, an observation)
```

One gap per round. Twenty small notes produce twenty small edits and no jump.

## Checkpoints

| after | question | on fail |
|---|---|---|
| SURVEYOR | does the blueprint, if built, deliver the commission? | re-survey |
| FOUNDER | do these ingots, all forged, add up to the commission? | re-found / add ingots |
| each ingot | does this ingot meet its own `:bar`? | re-smelt → SPLIT |
| FORGE (whole) | does the artifact meet `BAR.md`? | gap → addendum → extend → forge again |

The last row is the loop that matters most and the cheapest to build, because
both halves already exist.

## Loop control

No fixed round count in spirit, but real money is at stake, so: `--temper-rounds`
caps it, existing cost caps still bind, and a round that produces no new ingots
stops the loop rather than spinning.

## Live progress

`PROGRESS.md` is already the ledger. Each warden verdict appends to it: round,
level, verdict, gap, evidence. That is the window the gauntlet method asks for,
in a file slag already maintains.

## Waves

1. **Bar + warden + the FORGE checkpoint.** The root loop, end to end. Highest
   value, and it closes the "we are sure" question.
2. **Per-ingot `:bar` + the ingot checkpoint**, wired to re-smelt/SPLIT. This is
   where it becomes fractal.
3. **The SURVEYOR and FOUNDER checkpoints.** Cheapest to add once the warden
   exists; catches a doomed plan before any money is spent forging it.

## Open question

Default on or off. A warden pass costs a model call and tool time on every run.
Wave 1 ships it behind `--temper` so a default change is a separate, deliberate
decision.
