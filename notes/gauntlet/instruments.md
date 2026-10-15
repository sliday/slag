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
