---
name: slag_rs_event_and_render_surfaces
desc: Where the EngineEvent enum actually lives, the append-only contract on it, and the render/ module's terminal-free shape.
tags: [slag-rs, events, rendering, conventions]
sources: []
created: 2026-08-26T08:17:23Z
updated: 2026-08-26T08:17:23Z
---

# slag_rs_event_and_render_surfaces

Two facts about `slag-rs/` that cost time to discover, written down so
the next node reads instead of greps.

## The `EngineEvent` enum lives in `engine/mod.rs`, not `engine/events.rs`

The name misleads. `engine/events.rs` holds the **sink** and the
**renderer** — `spawn_jsonl_sink`, the narrator's card builder, the
per-variant feed text. The enum itself is declared in
`engine/mod.rs` (around line 223).

So: appending a variant or a field means editing `mod.rs`. Grepping
`events.rs` for the variant list finds only match arms.

### The append-only contract

Multiple nodes add to this enum in parallel. Append variants and fields
at the end of their block; never reorder or rename existing ones, or a
clean squash turns into a manual conflict.

**Every new field on an existing variant needs `#[serde(default)]`.**
The JSONL event log is read back by `slag status --json`, `slag runs`,
and `slag insights`, all of which parse logs written by older binaries.
A field without a default makes every historical line fail to
deserialize.

Worked example — `ToolResult` gained output counts and a duration:

```rust
ToolResult {
    name: String,
    ok: bool,
    preview: String,
    #[serde(default)]
    lines: usize,
    #[serde(default)]
    bytes: usize,
    #[serde(default)]
    ms: u64,
},
```

Counts measure the **full** tool output, not the truncated `preview`.
A collapsed one-liner exists to say how much it is hiding; measuring the
preview would have it describe itself.

Adding fields to a variant breaks every construction site and every
exhaustive pattern. In practice that is one production site
(`engine/agent.rs`) plus test helpers, and the match arms in
`events.rs`/`dashboard.rs` that need `, ..` added.

## `src/render/` is terminal-free by construction

Modules under `render/` turn forge data into renderable **shapes** —
kinds, spans, counts — and never touch a terminal. The caller maps those
onto ratatui styles (dashboard) or ANSI escapes (stream mode).

The payoff is testability: `render::diff`'s interesting behavior is
where its change-ratio threshold flips between word granularity and
full-line coloring, and that is asserted on plain data rather than by
driving a terminal.

Follow the shape when adding a renderer. A module that emits ANSI
directly cannot be tested without a PTY and cannot be reused by the
other view.

## `similar` crate traps

Three of them, all hit while building the word differ:

- **`grouped_ops(usize::MAX)` panics** — overflow inside the crate's
  `common.rs`. Use `diff.ops()` when nothing elides context.
- **`iter_inline_changes` needs a feature flag.** `iter_changes(op)`
  does not, and is enough when you run your own word diff over each op.
- **`from_words` emits separating whitespace as its own `Equal` token.**
  Two adjacent replaced words therefore render as Changed/Same/Changed —
  two highlights with a visible seam. Absorb a whitespace-only gap that
  sits between two changed runs before merging.

### Change ratios divide by both sides

A replacement contributes a deletion *and* an insertion. Scoring it
against one side's length counts the same edit twice, and ordinary
one-word renames cross a 0.4 threshold they have no business crossing.

`(deleted + inserted) / (len(old) + len(new))` — with shared text
counted on both sides — puts `let total = …` → `let count = …` at 0.25
where it belongs.

## Testing rendered output

Wide glyphs (⏳, ⚒) occupy two cells in a ratatui `TestBackend` buffer,
and the second cell reads as a space. `content.contains("⏳ text")`
fails on that padding. Assert on the glyph and the text separately, or
count occurrences.

## Palette

`tui.rs` exports COLD, WARM, HOT, BRIGHT, PURE, mirroring the `--slag-*`
variables in `website/src/main.css`. Change one, change the other — and
`website/` is outside most nodes' scope, so in practice: use the
constants, never edit their values.

WARM is red (`0xe06c75`). BRIGHT is yellow (`0xffd866`). The names do
not order by temperature the way the metaphor suggests, so read the
values before picking one for a severity tint.

## Tapping the whole event stream: `hooks.events`, not a smith

To consume every event a run produces, tap `EngineHooks::events`. It is
the one channel carrying **both** halves:

- pipeline events — `IngotStart`, `IngotDone`, `HeatTick`, `DuelVerdict`
  — emitted by `pipeline/forge.rs`;
- agent events — `ToolCallStart`, `ToolResult`, `Tokens`, `ContextGauge`
  — fanned in per-smith at `smith/native.rs` (~line 142), where one agent
  channel already splits to the JSONL sink, the dashboard hook, and the
  stderr narrator.

Tapping the smith's own channel instead looks tempting and is wrong for
anything correlating work to ingots: a smith's stream has no ingot
events, so tool calls arrive with nothing to attribute them to.

`hooks.events` is `Option<EventTx>` and it is `None` on the headless
path, so a new consumer wants both branches: tee when something already
holds the channel, take it outright when nothing does. `render/trace.rs`
`attach()` is the worked example.

The consequence for attribution: only ingot events carry an id, so with
several anvils on one channel you cannot tell which anvil a tool call
came from. Attribute when exactly one ingot is open and fall back to a
shared bucket otherwise, rather than guessing.

## Writing a JSON *array* sink: the offset hazard

JSONL forgives truncation; a JSON array does not — no closing bracket,
no reader will load the file. Two things bite:

**A hard exit skips async cleanup.** `shutdown::install_signal_handler`
ends in `std::process::exit(130)`, which never lets a tokio task finish.
A sink that closes its file on channel drain covers a clean exit and a
dashboard quit, and misses the shell Ctrl-C entirely. Register a
*synchronous* close with `shutdown::register` for that path.

**Two closers race, and the loser can silently undo the winner.** Guard
with a shared `AtomicBool` so the bracket is written once. Ordering is
the subtle half: a `File` handle tracks its own offset, so if a cleanup
appends `]` at the end and the async task then writes anything, that
write lands *on* the bracket and reverts the rescue. The async side must
claim the flag **before** its final writes and bail if it lost, not
merely skip the bracket.
