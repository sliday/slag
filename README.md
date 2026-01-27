# slag

[slag.dev](https://slag.dev)

**Smelt ideas, skim the bugs, forge the product.**

## Install

```bash
curl -fsSL https://slag.dev/slag -o /usr/local/bin/slag && chmod +x /usr/local/bin/slag
```
![slag-promo](https://github.com/user-attachments/assets/d12def06-6eab-4236-9634-bbbd09be6683)

## Quick start

```bash
# write your requirements
cat > PRD.md << 'EOF'
Build a REST API with user authentication, rate limiting,
and PostgreSQL storage. Include health check endpoint.
EOF

# forge
slag
```

Or pass the commission directly:

```bash
slag "Build a CLI tool that converts CSV to JSON"
```

This writes `PRD.md` for you and runs the full pipeline.

> **WARNING:** slag runs Claude with `--dangerously-skip-permissions`. This means Claude will execute shell commands, write files, and make changes to your system **without asking for confirmation**. Run it in a clean directory or container. Review the generated `PRD.md` before forging. You accept all risk.

---

A bash orchestrator for AI-powered development. Give it a product requirement, and it breaks it into verifiable tasks, executes them via Claude, and proves each one passed before moving on. No human review needed -- every task carries its own machine-verifiable acceptance test.

## Why

AI coding agents are powerful but chaotic. They lose context on long tasks, hallucinate completeness, and can't tell you whether their output actually works. Existing orchestrators add layers of abstraction (YAML configs, plugin systems, Docker containers) that fight the simplicity of just running shell commands.

slag takes a different approach:

- **One file.** A single bash script. No runtime, no dependencies beyond bash and `claude`.
- **S-expressions for state.** Each task is one line in a text file, parseable with `grep` and `sed`. No JSON/YAML parser needed.
- **Proof over trust.** Every task has a `:proof` field -- a shell command whose exit code determines pass/fail. `test -f file`, `npm test`, `curl -s url | grep -q pattern`. If it exits 0, the task is forged.
- **Automatic retry with feedback.** Failed tasks get retried with the error output fed back to the AI. Up to N attempts before giving up.
- **Parallel execution.** Independent tasks run on concurrent "anvils" (background subshells). Dependent tasks run sequentially.
- **No questions asked.** The AI is instructed to make expert decisions autonomously. If the surveyor's analysis contains questions, slag feeds it back with instructions to resolve them. Up to 3 self-iteration rounds.

## How it works

slag runs a 4-phase pipeline:

```
PRD.md --> SURVEYOR --> BLUEPRINT.md --> FOUNDER --> PLAN.md --> FORGE --> PROGRESS.md
 (ore)    (analyze)    (analysis)      (design)   (ingots)   (strike)    (ledger)
```

### Phase 1: SURVEYOR

Reads your `PRD.md` (the ore) and produces `BLUEPRINT.md` -- a deep analysis with architecture decisions, dependency graph, risk assessment, and forging sequence. Uses Claude's plan mode for thorough reasoning.

### Phase 2: FOUNDER

Reads the blueprint and casts S-expression ingots into `PLAN.md`. Each ingot is a single task:

```
(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5
       :proof "test -f package.json" :work "Initialize project with package.json")
```

### Phase 3: FORGE

The main loop. For each ingot:

1. **Pick** the next ore-status ingot
2. **Select smith** -- choose tools based on `:skill` tag (web gets Playwright, etc.) and `:grade` (high complexity gets plan mode)
3. **Strike** -- invoke Claude with the task description
4. **CMD** -- extract and run the shell commands from Claude's output
5. **Proof** -- run the `:proof` command
6. Pass: mark `:forged`, git commit. Fail: increment `:heat`, retry with error feedback. Max retries: mark `:cracked`, halt.

Independent ingots (`:solo t`) run on up to 3 parallel anvils. Dependent ingots (`:solo nil`) run sequentially after.

### Phase 4: ASSAY

Final report. Counts forged vs cracked ingots, shows a temperature bar, and exits 0 on full forge or 1 if anything cracked.

## Ingot fields

| Field | Values | Meaning |
|-------|--------|---------|
| `:id` | `"i1"`, `"i2"`, ... | Unique identifier |
| `:status` | `ore` / `molten` / `forged` / `cracked` | Lifecycle state |
| `:solo` | `t` / `nil` | Can run in parallel or must be sequential |
| `:grade` | 1-5 | Complexity; grade >= 3 uses plan mode |
| `:skill` | `web` / `api` / `cli` / `default` | Selects smith tools |
| `:heat` | 0-N | Current retry attempt |
| `:max` | 5-8+ | Max retries before cracking |
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the smith |

## Files

| File | Role |
|------|------|
| `PRD.md` | Requirements input (ore) |
| `BLUEPRINT.md` | Surveyor analysis output |
| `PLAN.md` | Ingot crucible (task list) |
| `AGENTS.md` | Learned recipes |
| `PROGRESS.md` | Work history ledger |
| `logs/` | Debug logs (slag heap) |

## Requirements

- bash 4+
- [Claude CLI](https://docs.anthropic.com/en/docs/claude-code) (`claude` command)

## License

MIT
