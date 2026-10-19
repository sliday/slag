# Instrument notes

Harness quirks found during the run. Every critic reads this first.

- Lead session runs worktree-isolated git guards: git commands that target another worktree are refused from inside an isolated session. Lane worktrees were created from the main checkout.
- `EnterWorktree` without `path` branches from `origin/main`, an unrelated v1 history. Always create worktrees from local refs.
- `pytest` is not installed. Python fixtures use stdlib `unittest`.
- macOS has no `timeout` binary by default; the bench implements its own watchdog.
- Terminal screenshots: `vhs` and `freeze` (charmbracelet) are installed at /opt/homebrew/bin.
- vhs 0.12.1 TUI capture: `Screenshot "rel/path.png"` needs a quoted path and silently writes nothing if it is unquoted; odd Width/Height (e.g. 825x465) makes the ffmpeg step fail ("render failed 234"), use even pixels; GIF encoding of 1650x930 is slow, use `Output x.txt`. 80x24 = 826x466, 160x48 = 1650x930 at FontSize 16. Run tapes in a per-run copy of the project (agents edit files). Codex swallows Enter sent 1s after typing (paste burst), use Sleep 2s. Bare `claude` here opens the agent-view dashboard and `--bare` fails auth, so use `claude --setting-sources project` with CLAUDE_CODE_CHILD_SESSION and related env vars unset; trust prompt needs Down then Enter.
- Web screenshots: `playwright` is not installed in website/ or globally; `npx playwright screenshot` cannot emulate reduced motion. Working recipe: symlink `/Users/stas/.npm/_npx/e78b33305587cb7c/node_modules` (playwright 1.56, no browsers) next to a .mjs script and launch `chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' })`; `newContext({ reducedMotion: 'reduce' })` works. CLS via a `layout-shift` PerformanceObserver in `addInitScript`.
- ffmpeg `tile=AxB` with `-frames:v 1` writes nothing (no error at `-v error`) when fewer than A*B frames are decoded; count frames first or drop `-frames:v 1`. `-vsync` is gone, use `-fps_mode passthrough`.
- A subagent that calls EnterWorktree can pin the LEAD session to that worktree too (seen 23:25 with builder-sandbox). Symptom: git -C on other worktrees refused. Fix: lead calls ExitWorktree keep. Tell builders to use absolute paths and never call EnterWorktree.
- Groq account limits: 8k tokens/min and 200k tokens/day. Critics and builders together exhausted the daily cap on 2026-10-15; Groq live checks after ~00:00 CEST that day are not evidence about slag.
- `cmd | grep ...` as a test gate hides cargo's exit code. Capture cargo's own status (`cargo test > log; echo $?`).
- `awk length` counts bytes under the C locale; box-drawing rules (3 bytes each) look 3x too long. Measure line width in characters (python3 len).
- Codex bench work dirs can contain a `codedb.snapshot` MCP artifact; strip it before any blind diff comparison (it decided a whole fx vote round).
- Blind TUI critics can mislabel which candidate is slag (tui-r2 called slag's idle-160x48 frame 'a logo on a blank screen'; it was not). Always check the critic's labels against the key file before using a ranking.
- 2026-10-17 ~11:00: the Cerebras account returned 402 (credits exhausted) during fix r2. Live Cerebras checks after this point fail for account reasons, not slag. Use openai, mistral, gemini, groq (8k TPM, 200k/day) or openrouter.
- 2026-10-17 11:37: smoothing lane (codex) may run a deliberate CPU load test (yes > /dev/null x16) for part of its run; wall-clock numbers measured by critics in that window can be inflated by CPU load (LLM latency is mostly unaffected).
- 2026-10-17 11:44: codex in lane-smooth (inside the main repo folder) also edited the MAIN checkout's README.md and CLAUDE.md (identical to its lane commit 0b31399). Lead saved the diff to tmp/main-checkout-stray-edit.patch and restored both files to HEAD. Briefs must name forbidden parent paths explicitly; check `git -C /Users/stas/Playground/slag status` after every codex run.
- TUI blind critics r2 and r3 both mislabeled one 160x48 group (r3: called slag's ASSAY frame 'Claude Code text summary'). Audit every label against the key. Also the blind set compares slag's commission fixtures (long, with retries) against Codex/Claude doing one small fix; from r4 the set must use slag fix on the same refs-tui project and task.
- Critics must copy the slag binary into their scratch dir before running (from fix-r3 on). The lead keeps merging lanes into the integration worktree, which rebuilds target/debug/slag under any critic that runs it in place.
