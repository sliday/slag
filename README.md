# slag

[![Release](https://img.shields.io/github/v/release/sliday/slag?label=release&color=ff9940)](https://github.com/sliday/slag/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-ffd866.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-e06c75.svg)](slag-rs/)
[![LLM: OpenRouter](https://img.shields.io/badge/LLM-OpenRouter-6b7385.svg)](https://openrouter.ai)

**Smelt ideas, skim the bugs, forge the product.**

A task orchestrator for AI-powered development. Give it a product requirement, and it breaks the work into verifiable tasks, executes them against a model on OpenRouter, and proves each one passed before moving on. No human review needed: every task carries its own machine-verifiable acceptance test.

One key, one command. slag talks to OpenRouter and nothing else. You do not install the Claude CLI, you do not log into a vendor tool, you do not pick a model.

## Quick start

```bash
# install (single binary)
curl -sSf https://slag.dev/install.sh | sh

# one-time setup: paste your OpenRouter key
slag key

# forge
slag "Build a REST API with auth and rate limiting"
```

That third line writes `PRD.md` for you and runs the full pipeline. Get a key at [openrouter.ai/keys](https://openrouter.ai/workspaces/default/keys).

Already have the key in your environment? Then skip `slag key` entirely:

```bash
export OPENROUTER_API_KEY=sk-or-...
slag "Build a CLI tool that converts CSV to JSON"
```

Running with an existing `PRD.md` in the directory? Run `slag` with no argument and it forges what is already there.

## The key

`slag key` is the whole configuration surface.

| Command | What it does |
|---------|--------------|
| `slag key` | Prompts for a key when there is none yet and you are on a terminal. Otherwise shows the active key (masked), where it came from, and the four models. |
| `slag key sk-or-...` | Verifies the key against OpenRouter, then saves it. |

Use the bare `slag key`. Passing the key as an argument leaves it in your shell history.

Verification happens before storage. A key OpenRouter rejects never gets written, so a typo fails in the twenty seconds you spent setting up rather than fifteen minutes into a forge. If OpenRouter is unreachable, slag warns and stores the key anyway (a key typed on a plane is still probably the right key).

Storage: `~/.config/slag/config.toml`, mode 0600. `OPENROUTER_API_KEY` in the environment always wins over the file, so CI can override a stored key without touching it. `$SLAG_CONFIG_DIR` moves the whole config directory.

`slag status` and `slag update` never ask for a key. They exist to inspect or repair a broken setup.

## Models

Every role defaults to `openrouter/auto`, OpenRouter's automatic router. It picks a live model per request, so a fresh key works with zero model configuration.

| Role | Used for | Env var | `config.toml` key | Flag |
|------|----------|---------|-------------------|------|
| work | Grade 1-2 ingots | `SLAG_MODEL_BASE` | `model_base` | `--model` |
| plan | Grade 3+ ingots, surveyor, founder | `SLAG_MODEL_PLAN` | `model_plan` | `--plan-model` |
| duel | Cast B in a twin-cast duel | `SLAG_MODEL_ALT` | `model_alt` | |
| judge | Assays duel casts | `SLAG_MODEL_JUDGE` | `model_judge` | `--judge-model` |

The two spellings are not interchangeable. slag reads the uppercase names from the environment only, and the lowercase keys from the file only. An uppercase name written into `config.toml` is ignored without a warning.

Any OpenRouter model id works:

```bash
slag --model anthropic/claude-sonnet-4.5 "Build a Slack bot"
```

Precedence runs flag, then environment variable, then config file, then `openrouter/auto`. To pin a model permanently, add it to `~/.config/slag/config.toml`:

```toml
openrouter_api_key = "sk-or-..."
model_base = "anthropic/claude-sonnet-4.5"
model_plan = "openai/gpt-5"
```

The file reads ten keys, all lowercase: `openrouter_api_key`, `model_base`, `model_plan`, `model_alt`, `model_judge`, `duel`, `duel_rounds`, `screenshot_cmd`, `max_cost_per_ingot`, `max_cost_per_run`. Anything else in the file is left alone and never read.

### MCP servers

An `[mcp]` table adds Model Context Protocol servers over stdio, one command per line:

```toml
[mcp]
filesystem = "npx -y @modelcontextprotocol/server-filesystem /tmp"
github = "gh-mcp --stdio"
```

slag spawns each server at forge start and hands the smith every tool it advertises, named `mcp__<server>__<tool>`, beside the built-in eight. A server that will not start is named in a warning and skipped; the forge runs on the natives. stdio transport only, so no HTTP, SSE, or OAuth servers.

`--auto` forces `openrouter/auto` on all four roles, overriding whatever is in your environment or config file. Explicit model flags still win over it. Because it resets `model_alt` too, it also switches duels off for that run: a duel needs two different models.

## Why

AI coding agents are powerful but chaotic. They lose context on long tasks, hallucinate completeness, and cannot tell you whether their output actually works. Existing orchestrators add layers of abstraction (YAML configs, plugin systems, Docker containers) that fight the simplicity of just running shell commands.

slag takes a different approach:

- **One binary, one key.** No runtime, no CLI to install first, no vendor login.
- **S-expressions for state.** Each task is one line in a text file, readable by you and by `grep`. No JSON/YAML parser needed.
- **Proof over trust.** Every task has a `:proof` field, a shell command whose exit code decides pass or fail. `test -f file`, `npm test`, `curl -s url | grep -q pattern`. Exit 0 means the task is forged.
- **Automatic retry with feedback.** Failed tasks get retried with the error output fed back to the model. Up to N attempts before giving up.
- **Parallel execution.** Independent tasks run on concurrent anvils. Dependent tasks run sequentially.
- **No questions asked.** The model is instructed to make expert decisions autonomously. If the surveyor's analysis contains questions, slag feeds it back with instructions to resolve them. Up to 3 self-iteration rounds.
- **Watchable and steerable.** `--tui` opens a Ratatui dashboard with the crucible, a live feed narrated in plain English, and a steer input: type a message, press Enter, and the running smith folds it in.
- **Spend caps.** `SLAG_MAX_COST_INGOT` and `SLAG_MAX_COST_RUN` put dollar ceilings on a single ingot session and on the whole run. Default is uncapped.

## How it works

slag runs a 4-phase pipeline:

```
PRD.md --> SURVEYOR --> BLUEPRINT.md --> FOUNDER --> PLAN.md --> FORGE --> PROGRESS.md
 (ore)    (analyze)    (analysis)      (design)   (ingots)   (strike)    (ledger)
```

### Phase 1: SURVEYOR

Reads your `PRD.md` (the ore) and produces `BLUEPRINT.md`, a deep analysis with architecture decisions, dependency graph, risk assessment, and forging sequence. Runs on the plan model.

### Phase 2: FOUNDER

Reads the blueprint and casts S-expression ingots into `PLAN.md`. Each ingot is a single task:

```
(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5
       :proof "test -f package.json" :work "Initialize project with package.json")
```

### Phase 3: FORGE

The main loop. For each ingot:

1. **Pick** the next ore-status ingot
2. **Select smith**, picking the model by `:grade` (grade 3 and up gets the plan model)
3. **Strike**, running an agent loop against OpenRouter with the task description
4. **Proof**, running the `:proof` command
5. Pass: mark `:forged`, git commit. Fail: increment `:heat`, retry with the error fed back. Max retries: mark `:cracked`.

Independent ingots (`:solo t`) run on up to 3 parallel anvils. Dependent ingots (`:solo nil`) run sequentially after.

High-grade ingots can go to a twin-cast duel: two casts build the same ingot in separate git worktrees, a judge model assays both, and the winner merges. Duels only fire when cast B runs a different model from the worker, since two rolls of the same dice cost triple the tokens and buy nothing. Set `SLAG_MODEL_ALT` to a different family to turn duels on for grade 3 and up, or force the behavior with `SLAG_DUEL=on` / `off`.

### Phase 4: ASSAY

Final report. Counts forged versus cracked ingots, shows a temperature bar, and exits 0 on a full forge or 1 if anything cracked. It prints on failed runs too, since a run that cracked an ingot is when you most need the cracked list.

## Architecture

```
                       slag (single Rust binary)
 ┌────────────────────────────────────────────────────────────────┐
 │  cli.rs ── main.rs ── pipeline/                                │
 │                        ├─ surveyor  PRD.md      → BLUEPRINT.md │
 │                        ├─ founder   BLUEPRINT.md → PLAN.md     │
 │                        ├─ forge     ingots → anvils (≤3 ∥)     │
 │                        │             ├─ duel (twin casts+judge)│
 │                        │             └─ resmelt (one rewrite)  │
 │                        └─ assay     forged/cracked report      │
 │                                                                │
 │  engine/  agent loop · tools (read/write/edit/grep/bash)       │
 │           recipes · judge · events → narrator                  │
 │  smith/   NativeSmith factories (work / plan / base)           │
 │  tui.rs · dashboard.rs   stream feed · Ratatui + steering      │
 │  config.rs   key store · model roles · spend caps              │
 └───────────────┬────────────────────────────────────────────────┘
                 │  HTTPS (chat completions, tool calls)
                 v
          OpenRouter API ── openrouter/auto (default)
```

No vendor CLI underneath: the engine is slag's own agent loop speaking the OpenRouter chat-completions API directly.

## Commands

| Command | Role |
|---------|------|
| `slag "commission"` | Write `PRD.md` and run the full pipeline |
| `slag` | Forge the `PRD.md` already in this directory |
| `slag key [KEY]` | Show setup, or verify and store a key |
| `slag status` | Show crucible state |
| `slag resume` | Resume an existing forge |
| `slag update` | Self-update to the latest release |

## Flags

| Flag | Effect |
|------|--------|
| `--tui` | Full-screen dashboard with crucible, live feed, and steering input |
| `--anvils N` | Max parallel anvil workers (default 3) |
| `--auto` | Force `openrouter/auto` on all four roles, ignoring pinned models |
| `--model MODEL` | Worker model |
| `--plan-model MODEL` | Planner model for grade 3+ ingots |
| `--judge-model MODEL` | Duel judge model |
| `--worktree` | Branch-per-ingot isolation. Not implemented yet, warns and runs in the shared checkout. |

## Environment

| Variable | Effect |
|----------|--------|
| `OPENROUTER_API_KEY` | The key. Overrides the stored one. |
| `SLAG_MODEL_BASE` / `_PLAN` / `_ALT` / `_JUDGE` | Per-role model ids |
| `SLAG_DUEL` | `auto` (default), `on`, `off` |
| `SLAG_DUEL_ROUNDS` | Override the duel round cap |
| `SLAG_MAX_COST_INGOT` | Dollar ceiling for a single ingot session (file key `max_cost_per_ingot`). Default uncapped. |
| `SLAG_MAX_COST_RUN` | Dollar ceiling for the whole run (file key `max_cost_per_run`). Default uncapped. |
| `SLAG_REASONING_EFFORT` | `low`, `medium`, `high` for models that take it |
| `SLAG_SCREENSHOT_CMD` | Shell command producing a screenshot for visual assay of web ingots |
| `SLAG_CONFIG_DIR` | Move the config directory off `~/.config/slag` |
| `SLAG_OPENROUTER_BASE` | Point slag at a proxy instead of OpenRouter |
| `SLAG_CHAR_BUDGET` | Transcript char budget before an agent session compacts (default 600000). Lower it for small-context models. |
| `NO_COLOR` | Set to anything to drop color from the output |
| `COLORTERM` | Read for `truecolor` / `24bit`; without it slag falls back to 256-color |

## Ingot fields

| Field | Values | Meaning |
|-------|--------|---------|
| `:id` | `"i1"`, `"i2"`, ... | Unique identifier |
| `:status` | `ore` / `molten` / `forged` / `cracked` | Lifecycle state |
| `:solo` | `t` / `nil` | Can run in parallel or must be sequential |
| `:grade` | 1-5 | Complexity; grade >= 3 goes to the plan model |
| `:skill` | `web` / `api` / `cli` / `default` | Work-type tag. Today only `web` changes anything: it notes Playwright in the prompt and turns on duel screenshots |
| `:heat` | 0-N | Current retry attempt |
| `:max` | 5-8+ | Max retries before cracking |
| `:smelt` | 0 / 1 | Re-smelt count. The founder writes 0; an ingot already re-smelted once carries 1 and cracks for good next time. |
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the smith |
| `:duel` | `t` / `nil` | Twin-cast override. `t` forces a duel, `nil` blocks one, absent defers to `SLAG_DUEL`. Sequential ingots never duel either way. |

The parser accepts these eleven fields and keeps any other `:key value` pair you add, writing it back unchanged.

## Files

| File | Role |
|------|------|
| `PRD.md` | Requirements input (ore) |
| `BLUEPRINT.md` | Surveyor analysis output |
| `PLAN.md` | Ingot crucible (task list) |
| `AGENTS.md` | Scratch notes. slag creates it holding one line, `## Alloy Recipes`, and reads the whole file into every ingot prompt (the survey, found, and re-smelt passes do not read it). After that only the smith writes to it, when the prompt asks it to jot a pattern down. |
| `PROGRESS.md` | Work history ledger |
| `logs/` | Debug logs (slag heap) |
| `recipes/<name>/RECIPE.md` | The real recipes. Each one is a markdown file with `---` frontmatter (name, description, optional requires_tools). slag indexes `./recipes/` and `~/.config/slag/recipes/` and offers the list to the smith. |

## Requirements

- An OpenRouter key
- `git` (slag commits each forged ingot)

`install.sh` reads the latest release tag from the GitHub API, downloads `slag-<arch>-<os>.tar.gz`, unpacks the `slag` binary into `~/.slag/bin`, and tells you to add that directory to your PATH. Releases currently carry one target: `slag-aarch64-apple-darwin.tar.gz` (macOS on Apple Silicon), alongside the same binary uncompressed and a `sha256.sum`. Everywhere else the download 404s, so build it:

```bash
cargo install --path slag-rs
```

## Legacy: slag.sh

`slag.sh` in this repo is the superseded v1 orchestrator. It shells out to the Claude CLI with `--dangerously-skip-permissions`, needs `claude` on your PATH, and gets no new features. It stays in the tree so old runs remain reproducible.

The current slag is the Rust binary described above. Install it with `curl -sSf https://slag.dev/install.sh | sh`.

## Development

The orchestrator lives in `slag-rs/`:

```bash
cd slag-rs
cargo test
cargo build --release
```

The site at [slag.dev](https://slag.dev) lives in `website/` (Vite, deployed to Cloudflare Pages).

## License

MIT
