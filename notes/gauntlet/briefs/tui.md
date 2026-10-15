Complexity: hard — terminal UI design across two renderers (stream mode and the Ratatui dashboard), plus a new deterministic replay path.

# Lane: impeccable TUI flow

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-tui (git worktree, branch gauntlet/tui). Work only here, with absolute paths.

Use the `impeccable` skill for design judgment (hierarchy, spacing, typography, color, motion, empty/error states, UX copy) and translate it to the terminal. Also consult `make-interfaces-feel-better` if useful.

## Context

slag is a Rust coding-agent orchestrator in `slag-rs/`. Read `CLAUDE.md` first. It has two renderers:
- Stream mode (default): `src/tui.rs` (banner, status lines, palette COLD/WARM/HOT/BRIGHT/PURE) and `src/render/` plus the narrator in `src/engine/events.rs` (`RenderOp`, `print_line`).
- Dashboard (`--tui`): `src/dashboard.rs`, Ratatui, reads `UnboundedReceiver<EngineEvent>`; crucible table, live feed, steering input.
Every run also writes its events as JSONL under `logs/` (`spawn_jsonl_sink` in `engine/events.rs`).

The whole user journey: first run with no key → `slag key` → a commission `slag "build X"` → SURVEYOR → FOUNDER (ingots cast into PLAN.md) → FORGE (parallel anvils, heats, retries, `:proof` checks, re-smelt) → warden verdict when `--temper` is on → ASSAY report → the dashboard takes the next commission. A new `slag fix <issue>` flow is being built in parallel by another builder; leave room for it (same events, "fix" framing), do not implement it.

## Goal

The slag terminal experience reads as one deliberate, calm, information-dense product at every step of the journey, in both renderers, at 80x24 and at 160x48. It should feel like the best agent TUIs (Codex CLI, Claude Code, opencode, lazygit-level craft) while keeping slag's own forge identity (the metallurgy language and the fire palette that mirrors `website/src/main.css`).

## Deliverables

1. **Replay first.** `slag replay <events.jsonl> [--speed N] [--tui] [--size WxH-hint optional]` replays a recorded run through the same renderers with zero network and no key. Deterministic: same input, same frames. This is the instrument the critics and the website video use, so it must be faithful to a live run.
2. **Fixture runs.** Record 2 real runs with a cheap fast model (keys in env: OPENROUTER_API_KEY, CEREBRAS_API_KEY, GROQ_API_KEY; never print them; spend ≤ $1): one small commission that forges several ingots including at least one retry, and one with `--temper`. Run them in a temp dir outside the repo. Commit the event logs under `slag-rs/tests/fixtures/runs/` (scrub absolute home paths). Add a replay test that renders a fixture without panicking.
3. **Screenshot instrument.** `slag-rs/scripts/tui-shots.sh <out_dir>` renders a fixed set of frames as PNGs with `vhs` (installed) or `freeze` (installed): stream mode and dashboard, at 80x24, 120x36 and 160x48, at fixed points of the journey (start, mid-forge with parallel anvils, a retry/crack, the warden verdict, the final assay, the no-key first run, `slag key`, `slag status`). Same inputs must give the same frames. Look at your own PNGs while you work (read them with the image tool).
4. **The flow itself.** Improve whatever the screenshots show is weak: hierarchy, alignment, truncation and wrapping at narrow widths, color discipline (few accents, meaningful), state clarity (what is running, what passed, what costs what, what needs me), progress and time feedback, error and empty states, the final report, keyboard discoverability in the dashboard, resize behavior, `NO_COLOR` and non-TTY output (CI logs must stay clean and greppable). No gratuitous animation; no flicker.
5. Keep the TUI palette in `tui.rs` in sync with `website/src/main.css` `--slag-*` variables. If you change a color, write the new values into `/Users/stas/Playground/slag/.claude/worktrees/lane-tui/notes/palette-change.md` for the website builder; do not edit `website/`.
6. `cd slag-rs && cargo test` passes. Set `CARGO_PROFILE_DEV_DEBUG=line-tables-only`. Update snapshot-style tests only when the change is deliberate, and say which.

## Paths you own (edit nothing else)

`slag-rs/src/tui.rs`, `slag-rs/src/dashboard.rs`, `slag-rs/src/render/**`, new `slag-rs/src/replay.rs`, `slag-rs/src/engine/events.rs` (rendering parts: `RenderOp`, narrator, `print_line`; do not change event variants), `slag-rs/src/cli.rs` (the `Replay` subcommand only), `slag-rs/src/main.rs` (the replay dispatch arm and banner/summary printing only), `slag-rs/tests/fixtures/runs/**`, `slag-rs/scripts/**`, new tests.

Do NOT touch `config.rs`, `engine/provider.rs`, `smith/`, `pipeline/`, `website/`, `bench/`, or the version in `Cargo.toml`.

## Hard rules

- Minimal diffs outside the renderers. Match the code style and comment density.
- Prose (UI copy, docs, comments, commits): no em dashes, active voice, short sentences.
- Commit on branch `gauntlet/tui` in your worktree with messages like `tui: one sentence`. Do not push. No version bump.

## Report (≤300 words)

Files changed; the replay CLI; where the fixture logs are; the screenshot script usage and its output dir; the five biggest changes with before/after PNG paths (keep a `before/` set from your first screenshot run under `/Users/stas/Playground/slag/.claude/worktrees/lane-tui/notes/shots/before/` and the final set under `.../after/`); known gaps.
