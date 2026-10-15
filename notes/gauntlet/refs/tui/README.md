# Reference TUI screenshots

Terminal: vhs, FontSize 16, Dracula, no padding. 826x466 px = 80x24 cells, 1650x930 px = 160x48 cells.
Task: "run the tests and fix the failing one" in a 3-file Python package with one failing unittest.
Tools: Codex CLI 0.162.1 (gpt-6.1-sol low), Claude Code 2.1.300 (Opus 5.5).

| File | Shows |
|------|-------|
| codex-idle-80x24.png | Codex start screen: version, cwd, "Ask Codex to do anything" composer, model and hint footer |
| codex-working-80x24.png | Codex mid-run: collapsed Ran/Failed tool cells, status line "Working (19s, esc to interrupt)" |
| codex-done-80x24.png | Codex final: Edited-file diff block, summary, "Worked for 1m 17s" divider, composer |
| codex-idle-160x48.png | Codex start screen at 160 cols |
| codex-working-160x48.png | Codex mid-run at 160 cols |
| codex-done-160x48.png | Codex final at 160 cols: red/green diff, command cells, summary, "Worked for 45s" |
| claude-idle-80x24.png | Claude Code start screen: logo, model/plan, cwd, notices, boxed prompt, mode footer |
| claude-working-80x24.png | Claude mid-run: Bash(...) tool call with "Running...", spinner line "Osmosing... (19s, tokens)" |
| claude-done-80x24.png | Claude final: markdown summary, "Churned for 40s" line, prompt |
| claude-idle-160x48.png | Claude start screen at 160 cols (top row of logo clipped by the pane) |
| claude-working-160x48.png | Claude mid-run at 160 cols: multiple Bash calls, inline diff, spinner |
| claude-done-160x48.png | Claude final at 160 cols: tool results, summary, "Churned for 31s" |
