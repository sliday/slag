# Gauntlet workbench: slag to production

Started 2026-10-15 19:00 CEST. Base: v2.9.0 (`b6948d3`). Integration branch: `gauntlet/production`.

Goal (owner's words): "take this to the production level with impeccable TUI flow and remotion video for the website. I need fast LLM-provider-agnostic harness for solving issues."

## Bars

| Piece | Bar | Mechanical part |
|---|---|---|
| Provider | aider/opencode provider coverage | One setting switches provider; same fixture solved live on 4+ providers; no OpenRouter call off OpenRouter; `cargo test` green |
| Fix (`slag fix`) | `codex exec` on the same bench | Held-out bench of 8 seeded-bug repos: pass rate ≥ codex, median wall time ≤ codex |
| TUI | Codex CLI and Claude Code screens | Blind A/B screenshots at 80x24 and 160x48 from a replayed real run; NO_COLOR and non-TTY clean |
| Video | Warp / Zed / Linear / Raycast launch videos | Blind contact-sheet A/B; webm ≤ 3 MB, mp4 ≤ 4 MB, 15-30 s, legible at 390 px, reduced-motion poster |

## Lanes

| Lane | Builder | Worktree | Status |
|---|---|---|---|
| provider | codex gpt-6.1-sol high | `.claude/worktrees/lane-provider` | building |
| bench (instrument) | kimi k3-256k | `.claude/worktrees/lane-bench` | building |
| fix | Claude (Opus) | `.claude/worktrees/lane-fix` | building |
| tui + replay | Claude (Opus) + impeccable | `.claude/worktrees/lane-tui` | building |
| video + site | Claude (Opus) + remotion | `.claude/worktrees/lane-video` | building |
| release (4 targets) | codex gpt-reserve medium | `.claude/worktrees/lane-release` | building |
| refs: TUI screenshots | Claude (Sonnet) | `notes/gauntlet/refs/tui/` | collecting |
| refs: launch videos | Claude (Sonnet) | `notes/gauntlet/refs/video/` | collecting |

## Rounds

(critic verdicts land here: round, piece, winner, biggest gap, evidence)

### 19:40 provider, build round 1 (lead verification, not a verdict)
- `cargo test`: 851 unit + 9 CLI pass.
- Live tool-call smoke through slag's provider code, real keys: openrouter 2185 ms, openai 1019 ms, cerebras 492 ms, groq 600 ms, gemini 3279 ms, mistral 708 ms PASS; xai FAIL (account out of credits, but slag labels it "invalid key (403)").
- Warnings: baseline 21, lane 24. Integration target: 0.
- Critic r1 (Fable, fresh) running.

## Instrument notes

See `instruments.md`.
