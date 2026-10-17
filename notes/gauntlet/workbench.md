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

### 00:05 provider r4 merged; video r3 sent
- Groq 429 TPM now classifies as rate limit; throttle lines print provider, role, model and wait. `--help` provider-agnostic. MERGED, suite green.
- Live Groq not verifiable today: the account hit Groq's daily cap (198,764 / 200,000 tokens), spent by critics and builders. A first attempt also hit a 650 s test timeout after the Groq model called undeclared tool names. Not re-testable until the cap resets.
- Video r3: make every on-screen CLI line and site claim match the real CLI (post-TUI transcript in `refs/real/`); slow the provider beat; fix the stale "Why One Key?" section.

### 00:20 sandbox lane merged
- macOS Seatbelt profile (writes allowed only in workspace, temp, tool caches); Linux bwrap, then Landlock via raw syscalls, else one warning. `sandbox = auto|off`, `SLAG_SANDBOX`, `--no-sandbox`, `sandbox_writable`.
- Builder evidence: `$HOME` probe blocked and named; `./ok`, `$TMPDIR`, `cargo --version`, python tempfile work; Linux tests pass in Docker with bwrap and with Landlock. Bench with sandbox on: rust-hawkconf 15 s, js-leaderboard 6 s, py-passcheck 3 s, all PASS.
- MERGED; suite green (946 unit). Round 2 sent: proofs and `slag fix` test/reproduce runs still run unconfined although the model controls them.

### 00:35 TUI, critic r1 (Fable, fresh, blind P/Q/R): WINNER bar
- Blind labels verified against the key. slag rank: idle-80x24 1st, idle-160x48 1st, working-80x24 1st, working-160x48 1st, done-80x24 3rd, done-160x48 3rd. Codex wins "done" by showing diff, passing command, one-line result.
- Critic caveat: not truly blind, every screen carries its product name.
- BIGGEST_GAP: done screen contradicts itself: "✗ GOAL NOT MET" over a 100% bar (`tui.rs:274`), "✓ i1 forged" while the judge says the proof never ran. Sent as TUI r2 with the non-TTY padding bug (`tui.rs:114`, rows padded to 227-242 columns when piped).
- Held: dashboard status "0s · 0 tok" vs header "0:59 · 37k tok"; crucible titles truncated at 80x24, panel empty at 160x48.
- Live: `slag fix --provider cerebras` fixed a planted bug in 3-4 s; the fix summary screen "earns trust" (critic).

### 00:50 sandbox r2 merged
- Proofs, CMD lines (forge, duel) and every `slag fix` test run now run in the sandbox; blocked writes surface as "exit 1 (write blocked by slag's sandbox: <path>)". Hooks and `screenshot_cmd` stay outside (user-written).
- Severe Linux bug found by the sandbox builder in `slag fix`'s timeout path: `Command::new("kill").args(["-KILL", "-<pid>"])` with procps `kill` signalled every process the user owns. Fixed via the shell builtin (`repo.rs:291`), regression test proves a bystander survives. Lead checked the other kill sites: `kill -0 <pid>` probes only; `tools.rs` already used the builtin.
- MERGED; 951 unit tests green. Critic sandbox-r1 (Fable) hunting escapes (symlinks, `~/.cargo/bin` on PATH, git config, rc files, proofs, fix test scripts).

### 01:05 video r3 landed; provider r5 (pricing) building
- Every on-screen line now from real runs: cerebras `slag fix` on js-leaderboard (FIXED, 5 s), real `slag key --provider` output for 7 presets at 0.55 s each, `SLAG_MODEL_PLAN=openai::gpt-4.1-mini`, a real `slag replay` of a commission with a re-smelt. Site copy rewritten from `--help` and README; "Why One Key?" replaced; video full-bleed on phones.
- Sizes: webm 1,593,032 B, mp4 3,635,957 B (budget 4 MB), 28.8 s, seam RMSE 0. Critic video-r2 (fresh, blind + truth check) running.
- Builder-reported bug: after a mid-run 429, `--temper 1` printed no warden verdict and no "goal unverified" note (silent skip of the goal check). Lead root cause: not the 429. `#[arg(trailing_var_arg = true)]` (`cli.rs:21`) swallowed `--temper 1` into the commission text, so temper stayed 0. Fixed by the lead in `c8f117e` (flags parse before or after the commission; test pins it). Suite green, 952 unit.
- Provider r5: dated built-in price table for preset models, `[pricing]` override, token counts instead of `-`, no OpenRouter fetch off OpenRouter.

### 01:20 TUI r2 merged
- Done screen: verdict first, then gap and next command, then evidence (per-proof exit codes, diff stat, commits, branch @ HEAD). No bar on the assay. ✓ only when a proof outcome was recorded. Same layout for `slag fix` and the dashboard's final state. Piped output: `$COLUMNS` or 80, no escapes.
- Lead check, real `slag fix` piped on py-invoice-dates (cerebras): FIXED in 4 s; summary shows proof exit 0, "without the fix: exit 1, so the new test catches the bug", diff, branch. Max line 79 characters, 0 escape bytes. (awk said 227: it counts bytes.)
- Critic tui-r2 (fresh, blind) running, including a real `--temper 1` commission.

### 01:30 sandbox, critic r1 (Fable, fresh, escape hunt): WINNER bar
- Linux (bwrap 0.8.0 and Landlock, privileged Docker): every vector blocked; only workspace and /tmp writable.
- macOS blocked: symlink to $HOME, hard link to ~/.zshrc, ~/.npmrc, `git config --global`, ~/.zshrc, ~/.ssh, ~/.claude, LaunchAgents, nohup children, `launchctl submit`, proof writes, model-edited `slag fix` test scripts.
- macOS ESCAPED: ~/.cargo/bin (on PATH), ~/.cargo config, ~/.rustup, ~/Library/Caches; and the git hook plant: model writes `.git/hooks/pre-commit`, slag's own unsandboxed commit (`proof.rs:66-74`) runs it.
- Builds pass under Seatbelt, bwrap and Landlock.
- BIGGEST_GAP sent as sandbox r3 with the cache allowlist, theme "nothing written inside may run outside": neutralize hooks and command-bearing git config in every slag-run git call; read-only `.git/hooks` and `.git/config` for model commands; data-only cache subdirs; this user's $TMPDIR only.
- Held: MCP servers spawn unsandboxed (`engine/mcp.rs:224`); user-configured, document it.

### 01:40 video, critic r2 (Fable, fresh, blind + truth): WINNER ours
- Blind C > E > B > D > A; key: C = slag (verified), E = Copilot CLI, B = Zed, D = Charm Crush, A = Charm Wish.
- TRUTH pass: every fix row matches a real run log; `slag key --provider` panels match the built binary; forge rows match `slag replay ... --size 48x200` line for line.
- Budgets pass (webm 1.59 MB, mp4 3.64 MB, 28.8 s, no audio). CLS 0.005 desktop, 0 mobile; full-bleed at 390 px; reduced motion shows the poster.
- BIGGEST_GAP: fix-scene captions hold 1.0-1.3 s under a scrolling terminal. Sent as video r4, with a re-recorded forge beat (real `--temper 1` warden verdict; the old "crack" was a 429).
- Held for smoothing: README still says "Duels only fire when cast B runs a different model", contradicting the CLI's duel line.

### 01:55 provider r5 (pricing) merged
- 33 dated price entries with sources; `[pricing]` override; unknown prices print token counts; no catalog fetch off OpenRouter.
- Conflicts with TUI r2 in `fix.rs` and `echo.rs` resolved by the lead: kept the TUI r2 layout, fed its `run` row from `pricing::format_cost(&self.usage)`. Test functions in `src/`: merged 970 vs provider lane 966 (none lost). 968 unit tests pass.
- Live after merge: `slag fix` on js-user-pager, cerebras, 4 s, `run wall 4s · api 2s · tools 0s · ≈ $0.0232 est.`
- Lead verified the Cerebras price against cerebras.ai/pricing: Qwen 3.8 27B $0.99/M in, $1.49/M out (~1,850 tok/s).

### 10-15 21:46 to 10-17: Claude session limit
- Claude subagents hit the account session limit mid-round (sandbox r3, video r4, TUI critic r2, fix critic r1). Resumed after the reset with the same agents.
- Lead found a hang: `slag replay --at <point> --size WxH` held the frame for 24 h even with no TTY; critics left such processes running for ~2 h. Fixed in `d355d89`: only an interactive terminal holds an open frame; text and piped replays exit (0.9 s). Suite green.
- Critic provider-r4 (fresh) started: Groq's daily cap has reset, so Groq gets its first fair live check on this build; also checks spend estimates against published prices.

### fix, critic r1 (Fable, fresh): WINNER bar (diff quality only)
- Bench (held-out, instrument proven): cerebras 16/16 median 5.5 s p90 14 s; openai 15/16 median 13 s p90 48 s; gemini 16/16 median 33 s p90 73 s. Codex: 8/8, median 80 s, p90 141-155 s.
- Blind diff votes, slag (cerebras) vs codex: fx 0-8; critic 0-6 with 2 ties. Source fixes: correct root cause, minimal, every time. Tests: misplaced, thin, one invalid (BSD `touch -t` with 14 digits).
- Instrument save: a `codedb.snapshot` artifact in codex's work dirs decided the first fx round; stripped and re-voted.
- BIGGEST_GAP: regression test quality. Sent as fix r2 with the tautological-reproduction case (openai rust-hawkconf patched the symptom; its test matched its own patch), `fix.rs:413` silent continue, and "Read 3 files" counting calls.

### TUI, critic r2 (Fable, fresh, blind): WINNER bar (narrower)
- Key check by lead: critic mislabeled idle-160x48 (slag = P; it called slag's frame "a logo on a blank screen", which the frame is not). Other 5 labels correct.
- slag rank: idle-80 1st, working-80 1st, working-160 1st, done-80 1st, done-160 2nd, idle-160 unreliable (mislabel).
- BIGGEST_GAP: raw escape bytes on the `slag fix` done screen (TTY): literal `[38;2;224;108;117m✗[0m` (`fix.rs:1079`). Sent as TUI r3 with header/footer counter contradiction and judge prose that reads like slag's own claim.
- Routed to fix builder: reproduce leaves stale `__pycache__` (same size, same mtime second), so the user's tests FAIL right after slag prints FIXED.
- New finding outside TUI: a word-count commission (`--temper 1`, cerebras) took 7:56 and 2.3M tokens, ≈ $2.44 est.; one retry self-inflicted (slag's own commit dirtied PLAN.md against a clean-tree bar). Codex read-only cost analysis running.

### cost analysis of the wc commission (codex, read-only)
- 2,276,015 tokens (1,895,336 prompt, 380,679 completion), 149 calls, 476 s; matches the tape ledger exactly. Smith 1.43M, warden 649k (11 sessions), founder 133k, surveyor 60k.
- Drivers: coordinator-file rollback 837k (37%); full conversation replay +735k input; invented bar constraints ≥ 289k. Report: `cost-analysis-wc-commission.md`.
- Target: 20-40k tokens, 15-30 s. New lane `efficiency` (codex, hard): acceptance ≤ 200k tokens and ≤ 120 s on a real rerun, plus a second commission.

### warden integrity bug (found by the video builder while recording)
- `slag --temper 2 resume` after "goal not met: pip install . fails (PEP 668)": no ingots cast for the gap; "warden round 1" ran `edit greet_pkg/tests/test_greet.py`, `edit greet_pkg/greet_pkg/greet.py`, `pip install pytest-cov`, then ruled "goal met"; assay FORGED. The judge edited the work and approved its own edits. Logs: `/Users/stas/.claude/jobs/f353e9d3/tmp/video-r4/runs/resume.log`, repo `runs/greet`.
- Same resume: "warden returned no readable verdict" on the blueprint; `--temper 2` did not forge the gap although the done screen's `next` row promised it.
- Queued as efficiency r2 (owns warden.rs and smith tools): warden strictly read-only (no edit/write tools; shell under a read-only sandbox profile), resume honors the gap.

## Integration checklist (smoothing)
- Compiler warnings: baseline 21; target 0.
- `shutdown.rs` tests drain every hook in the process; isolate them.
- `spend -` for providers without pricing; `--help` OpenRouter wording.
- Video on-screen CLI lines must match the final real output.
- Website Features and "Why One Key?" sections still OpenRouter-only.
- Linux without `rg`: `glob_matches_patterns_with_directory_components` fails (sandbox on or off). Glob needs a fallback or the test must skip.
- ~~Cost: `spend -` for every non-OpenRouter provider~~ done in provider r5.
- README duel sentence contradicts the CLI's duel line and the site.

## Instrument notes

See `instruments.md`.
