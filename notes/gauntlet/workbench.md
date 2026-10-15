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

### 21:25 bench (instrument), lead verification
- 8 tasks: 3 py (unittest), 3 js (node:test), 1 rust, 1 sh; 3 easy, 3 medium, 2 hard.
- Floor noop 0/8, ceiling reference 8/8, every hidden suite fails on the buggy repo.
- Leak found by lead: adapters got `bench/issues/<task>/ISSUE.md` (beside `hidden/` and `reference.patch`) and `BENCH_TASK_DIR`. Fixed by kimi; probe adapter now checks env and paths. MERGED.
- Codex baseline (gpt-6.1-sol, medium): **8/8, median 80 s, p90 155 s.** This is the bar for `slag fix`.

### 21:25 loop lane (codex), lead verification
- Root cause: duplicate ingot ids (`crucible.rs:141` updated only the first `i1`, leaving a twin queued); `forge.rs:206` fetched the cracked first match; `duel.rs:102` `.max(1)` forced a paid round after heat exhaustion.
- Fix rejects duplicate ids, blocks terminal ingots, stops passes without progress. Mock regression test. 829 + 8 tests pass. MERGED.

### 21:25 provider round 2 (codex), lead verification
- JSON-content tool-call recovery; provider tool-validation 400 goes back to the model; `SLAG_OPENROUTER_BASE` now applies to openrouter only.
- Builder's live table: cerebras `qwen-3.8-27b` FORGED 15 s, openai `gpt-4.1-mini` 18 s, groq `openai/gpt-oss-120b` 270 s (8k TPM account limit).
- Integration branch after merging bench + loop + provider: 866 unit + 13 integration tests pass. Critic r2 (fresh) running on the integrated build.

### 22:05 fix lane landed, video r2 landed
- Fix builder smoke (its own fixtures, not the bar): slag 10-12 s per fix vs codex 74-78 s, about $0.001 per fix. Verdict comes from harness-run tests; deleted tests fail the attempt; reproduction proven by reverting the fix.
- MERGED into integration (README table conflict resolved by hand). 891 unit + 17 integration tests pass.
- Bench adapter `bench/agents/slag-fix.sh` wired to the real CLI. Lead instrument check: `py-passcheck` PASS in 6 s on cerebras `qwen-3.8-27b`. Transcript: `refs/real/slag-fix-cerebras-py-passcheck.txt`.
- Critic fix-r1 (fresh) running the full bench, 2 runs, several providers, blind diff votes vs codex.
- Video r2: poster now equals frame 0 (source RMSE 0); video opens on the finished fix. webm 980 KB, mp4 1.5 MB. On-screen `slag fix` lines still guessed; reconcile after the TUI lane merges, since TUI changes the stream output.

### 22:20 provider, critic r2 (Fable, fresh): WINNER bar (gap moved)
- b: 6/6 FORGED: mistral 7 s, cerebras 19 s, openai 20 s, openrouter 61 s, groq 233 s (8k TPM throttle), gemini 248 s. c: proxy saw only the active provider's host; `SLAG_OPENROUTER_BASE` trap no longer leaks. d: cerebras work + openai plan + mistral judge forged in 19 s. f: every run terminated.
- BIGGEST_GAP: deterministic provider errors (bad key, unknown model, no credits) burn 5 heats plus a re-smelt; messages lack provider name and remedy; xAI billing 403 labelled "invalid key". Sent as provider r3.
- Second tier, routed to loop lane r2: founder marked ingots `forged` with no proof run (mistral run); smith read slag's own `logs/` (gemini burned 1.48M tokens).
- Competitor: slag ties aider on time-to-first-fast-run (3 steps each), beats opencode.

### 22:40 loop r2 merged; data-loss race found in `slag fix`
- Loop r2: founded ingots always enter as `ore` (`founder.rs:123`); broad file search prunes `logs/`, `.git`, `slag-anvil-*` (`tools.rs:1360`). MERGED.
- Lead found a flaky test, then a real race behind it: `reproduce()` registers a global shutdown rescue, then writes the pre-fix originals. A shutdown in between (concurrent test, or Ctrl-C) restores the fix first and the originals after, so the user's file ends without the fix. Seen 1 in 6 full suite runs (`fix.rs:1123`, left "1", right "2"). Sent to the fix builder.
- Fix critic r1 partial: cerebras 16/16, median 5.5 s, p90 14 s; openai gpt-4.1-mini 15/16, median 13 s (one confident wrong fix stamped FIXED). Codex bar: 8/8, median 80 s.

### 23:00 provider r3 + race repair merged
- Provider r3: auth, billing and invalid-request errors stop at the first call (exit 1, heat 0, smelt 0); messages name provider, role, model and remedy. Critic r3 (fresh) running.
- Race repair: swap and rescue share one lock with phases Armed/Swapped/Done/Rescued; tests use a local rescue slot. Lead ran the full integration suite 10 times: 10/10 green (903 unit + 22 integration).

### 23:20 provider, critic r3 (Fable, fresh): WINNER bar, narrowly
- e passes: groq bad key 0.21 s, xAI billing 0.53 s with URL, lmstudio no server 1.86 s with start hint, unknown model stops at first smith call (heat 0, smelt 0, exit 1).
- f passes: `slag fix --provider cerebras` on a fresh 2-file repo: 7.5 s, correct even-median fix + 3 tests, only api.cerebras.ai contacted.
- BIGGEST_GAP: regression from r3: Groq TPM 429 body contains ".../settings/billing", classified as billing, run dies in 3 s. Sent as r4 together with the `--help` OpenRouter wording (third round in a row).
- Critic: "slag would win if Groq ran." Install-to-fast-run: slag 3 commands, aider 4, opencode install + TUI steps.
- Gemini run: smith ran `find /Users/stas ...` outside the workspace. New lane `sandbox` (Claude builder; codex cannot nest Seatbelt): OS-level write confinement for the shell tool.

### 23:50 TUI lane merged
- `slag replay <tape> --at <point> --size WxH` freezes deterministic frames; every run writes `logs/tape-*.jsonl`; stream view drawn from events with a pinned footer; failed heats say why; assay leads with the verdict.
- Integration bug from TUI x provider r3: fatal error printed 2-3 times, warden failure labelled "surveyor failed". Fixed by the TUI builder (71283e3). Suite green with cargo's own exit code: 928 unit + 27 integration.
- Lead note: an earlier merge commit went in while 2 tests failed (grep masked cargo's exit code). Fixed by the next merge; checks now read cargo's exit status.
- Non-TTY `slag fix` log pads tool rows to ~220 columns; fix flow shows the commission banner. Left for the TUI critic.
- Critic tui-r1 (Fable, fresh) running: blind P/Q/R vs Codex CLI and Claude Code at 80x24 and 160x48, then the real build.

## Integration checklist (smoothing)
- Compiler warnings: baseline 21; target 0.
- `shutdown.rs` tests drain every hook in the process; isolate them.
- `spend -` for providers without pricing; `--help` OpenRouter wording.
- Video on-screen CLI lines must match the final real output.
- Website Features and "Why One Key?" sections still OpenRouter-only.

## Instrument notes

See `instruments.md`.
