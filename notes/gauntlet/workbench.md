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
| refs: TUI screenshots | Claude (Sonnet) | `notes/gauntlet/refs/tui/` | done (12 shots) |
| refs: launch videos | Claude (Sonnet) | `notes/gauntlet/refs/video/` | done (4 videos) |
| loop (runaway forge) | codex gpt-6.1-sol high | `.claude/worktrees/lane-loop` | building |

## Rounds

(critic verdicts land here: round, piece, winner, biggest gap, evidence)

### 19:40 provider, build round 1 (lead verification, not a verdict)
- `cargo test`: 851 unit + 9 CLI pass.
- Live tool-call smoke through slag's provider code, real keys: openrouter 2185 ms, openai 1019 ms, cerebras 492 ms, groq 600 ms, gemini 3279 ms, mistral 708 ms PASS; xai FAIL (account out of credits, but slag labels it "invalid key (403)").
- Warnings: baseline 21, lane 24. Integration target: 0.
- Critic r1 (Fable, fresh) running.

### 19:55 release, rounds 1-2 (lead verification by running it)
- r1: full `--build` makes 8 assets; Linux binaries print `slag 2.9.0` in `debian:stable-slim` (amd64 and arm64). Defect: `--build --target X` deleted the other targets' assets (`publish-release.sh:126`).
- r2 after fix list: partial rebuild keeps all 8, `shasum -c` OK, `./slag` stays arm64, `--check` exits 1 with a missing asset and 0 when complete. MERGED into `gauntlet/production`.

### 20:45 provider, critic r1 (Fable, fresh): WINNER bar
- Forged end to end: openai 19 s, mistral 7 s, openrouter 41 s, gemini 107 s. Cerebras CRACKED (finish call sent as plain JSON content, "no CMD" every heat). Groq died in 2 s (provider 400 on a malformed tool call treated as fatal).
- No openrouter.ai host contacted under other providers (CONNECT proxy logs). Mixed roles route correctly.
- BIGGEST_GAP: the two fast presets do not forge. Sent as round 2, plus a security defect: `SLAG_OPENROUTER_BASE` sent the Cerebras key to the OpenRouter base URL.
- Pre-existing defect found: a truly cracked ingot re-queues and duels forever (135 rounds in 12 min). New lane `loop` (codex) owns it.
- Second tier held for later rounds: errors lack provider name and remedy; unknown model burns 5 heats; `--help` still says OpenRouter.

### 20:45 video, build round 1 (lead check)
- ffprobe: 24.000 s, 1600x900, 30 fps, no audio; webm 826,010 B (budget 3 MB), mp4 1,246,275 B (budget 4 MB). Critic r1 running blind vs 4 reference videos.

### 21:00 video, critic r1 (Fable, fresh, blind): WINNER ours
- Blind ranking D > C > E > B > A. Key: D = slag, C = Copilot CLI, E = Charm Crush, B = Zed, A = Charm Wish. Critic identified D correctly after the reveal.
- Reason D won: command typed by 1 s, failing test, patch and passing test by 4 s, one caption per beat, captions legible at 390 px.
- Budgets pass. CLS desktop 0.006, mobile 0.076 (font swap above the figure, not the video). Reduced motion: paused at t=0, poster shown.
- BIGGEST_GAP: poster (draft-PR state at 7.5 s) differs from frame 0 (empty prompt): a content pop on every load. Sent as round 2.
- Held: mobile embed 341 px instead of full bleed; provider names flip every 0.27 s, too fast to read; font-swap CLS.
- Open: on-screen `slag fix` output is guessed until the fix lane lands.

## Instrument notes

See `instruments.md`.
