# Instrument notes

Harness quirks found during the run. Every critic reads this first.

- Lead session runs worktree-isolated git guards: git commands that target another worktree are refused from inside an isolated session. Lane worktrees were created from the main checkout.
- `EnterWorktree` without `path` branches from `origin/main`, an unrelated v1 history. Always create worktrees from local refs.
- `pytest` is not installed. Python fixtures use stdlib `unittest`.
- macOS has no `timeout` binary by default; the bench implements its own watchdog.
- Terminal screenshots: `vhs` and `freeze` (charmbracelet) are installed at /opt/homebrew/bin.
