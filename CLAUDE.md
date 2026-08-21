# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**slag** is a Rust task orchestrator that uses metallurgical metaphors to manage AI-powered development workflows. It breaks requirements into S-expression "ingots" and forges them through an agent loop with automatic retry and proof-based verification.

slag is OpenRouter-only. There is no Claude CLI dependency and no vendor SDK: the engine speaks the OpenRouter chat-completions API directly. The single prerequisite is an OpenRouter key.

The repo has three layers:

1. **`slag-rs/`**: The orchestrator, a Rust binary shipped through GitHub Releases
2. **`website/`**: The Vite site for slag.dev, deployed to Cloudflare Pages
3. **`slag.sh`**: The superseded v1 bash orchestrator, kept for reference only (see Legacy below)

## Orchestrator Architecture (slag-rs/)

### 4-Phase Pipeline

1. **SURVEYOR**: Reads `PRD.md` (ore), produces `BLUEPRINT.md` on the plan model
2. **FOUNDER**: Reads blueprint, generates S-expression ingots in `PLAN.md`
3. **FORGE**: Picks ore, runs an agent session with tools, verifies with `:proof`, commits on pass
4. **ASSAY**: Final report with pass/fail status. `pipeline::run` calls it on every outcome except `Cancelled`, so a cracked run still prints its counts.

### Ingot S-Expression Format

```
(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f file" :work "Task description")
```

`sexp/parser.rs` parses eleven fields and preserves anything else in `Ingot::extra`:

| Field | Values | Meaning |
|-------|--------|---------|
| `:id` | "i1", "i2", ... | Unique identifier; an ingot without one does not parse |
| `:status` | ore / molten / forged / cracked | Task lifecycle state |
| `:solo` | t / nil | Can run in parallel (t) or must be sequential (nil) |
| `:grade` | 1-5 | Complexity; grade >= 3 uses the plan model |
| `:skill` | web / api / cli / default | Work-type tag; `frontend`/`ui`/`css`/`html` also map to web, anything unknown to default |
| `:heat` | 0-N | Current retry attempt |
| `:max` | 5-8+ | Max retries before cracking |
| `:smelt` | 0 / 1 | Re-smelt count. `resmelt.rs` refuses a second pass once this is >= 1 |
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the smith |
| `:duel` | t / nil | Twin-cast override: t forces, nil blocks, absent defers to `DuelMode` |

### Source Layout (slag-rs/src)

| Path | Role |
|------|------|
| `main.rs` | Entry point, subcommand dispatch, key gate before any forge |
| `cli.rs` | clap definitions for flags and subcommands |
| `config.rs` | `EngineConfig`, key storage and verification, model resolution |
| `engine/` | OpenRouter provider, agent loop, tools, judge, recipes, events, MCP client |
| `smith/` | `NativeSmith` factories (`make_smith`, `make_plan_smith`, `make_base_smith`) |
| `pipeline/` | surveyor, founder, forge, duel, resmelt, assay |
| `anvil/` | Worktree helpers. The parallel fan-out itself lives in `pipeline/forge.rs` |
| `crucible.rs`, `sexp/` | Ingot parsing and crucible state |
| `tui.rs`, `dashboard.rs` | Stream-mode output and the Ratatui dashboard |
| `update.rs` | Self-update from GitHub Releases (`sliday/slag`) |

### Project Files the Pipeline Reads and Writes

| File | Role |
|------|------|
| `PRD.md` | Requirements input (ore) |
| `BLUEPRINT.md` | Surveyor analysis output |
| `PLAN.md` | Ingot crucible (task list) |
| `AGENTS.md` | Scratch notes. `pipeline/mod.rs` writes the one-line `## Alloy Recipes` stub; `flux.rs` reads it into every ingot prompt, not the survey/found/re-smelt ones. No code appends to it. |
| `PROGRESS.md` | Work history ledger |
| `logs/` | Debug logs (slag heap) |
| `recipes/<name>/RECIPE.md` | The recipes proper (`engine/recipes.rs`): frontmatter-fenced markdown discovered under `<root>/recipes/` and the config dir, indexed into the prompt and loadable by name |

### Configuration

The key is the only required setup. Bare `slag key` prompts for one when none exists and stdin is a TTY, and otherwise prints the status panel; `slag key <KEY>` verifies and stores the argument, which is the form that leaks into shell history. `OPENROUTER_API_KEY` in the environment overrides the stored one. Storage is `~/.config/slag/config.toml` at mode 0600, or `$SLAG_CONFIG_DIR/config.toml`.

Every model role defaults to `config::AUTO_MODEL` (`openrouter/auto`). Per-role overrides resolve flag, then env, then config file:

| Role | Env | `config.toml` key | Flag |
|------|-----|-------------------|------|
| work | `SLAG_MODEL_BASE` | `model_base` | `--model` |
| plan | `SLAG_MODEL_PLAN` | `model_plan` | `--plan-model` |
| duel cast B | `SLAG_MODEL_ALT` | `model_alt` | |
| judge | `SLAG_MODEL_JUDGE` | `model_judge` | `--judge-model` |

The env and file spellings are separate lookups in `config::EngineConfig::load`. `parse_config_lines` only ever matches the lowercase keys, so an uppercase name in the file is dead text. The file understands eight top-level keys: `openrouter_api_key`, `model_base`, `model_plan`, `model_alt`, `model_judge`, `duel`, `duel_rounds`, `screenshot_cmd`.

### MCP servers

`parse_config_lines` tracks `[section]` headers and namespaces the keys under them, so an `[mcp]` table holds one stdio server per line:

```toml
[mcp]
filesystem = "npx -y @modelcontextprotocol/server-filesystem /tmp"
github = "gh-mcp --stdio"
```

`engine/mcp.rs` spawns each server once per process at forge start, handshakes it (`initialize` → `notifications/initialized` → `tools/list`), and re-exports its tools as `mcp__<server>__<tool>` beside the native eight. `ToolBox::all_specs()` is natives plus MCP; `ToolBox::specs()` stays natives-only. A server that will not spawn or handshake inside 15s is named in a warning and skipped, never fatal. Recipes gate on these names through `requires_tools` unchanged. Scope is stdio only: no HTTP, no SSE, no OAuth.

Env-only knobs: `SLAG_DUEL` (auto/on/off), `SLAG_DUEL_ROUNDS`, `SLAG_REASONING_EFFORT`, `SLAG_SCREENSHOT_CMD`, `SLAG_OPENROUTER_BASE`, `SLAG_CONFIG_DIR`, `SLAG_CHAR_BUDGET` (agent.rs compaction budget), plus `NO_COLOR` and `COLORTERM` in `tui.rs`.

Behavior constants in `config.rs`: `MAX_ANVILS = 3` (parallel ingot slots), `HIGH_GRADE = 3` (plan-model threshold), `MAX_ITERATE = 3` (self-iteration rounds).

`DuelMode::Auto` duels grade 3 and up, and only when `model_alt` differs from `model_base`. With everything on `openrouter/auto` that gate stays shut, so the default never pays 3x tokens to duel a model against itself.

### Commands

```bash
cd slag-rs
cargo test              # keep this green
cargo build --release
```

Binary surface: `slag "commission"`, `slag key [KEY]`, `slag status`, `slag resume`, `slag update`. Flags: `--tui`, `--anvils N`, `--auto`, `--model`, `--plan-model`, `--judge-model`, `--worktree` (parses, warns, not implemented).

## Website (website/)

### Commands

```bash
cd website
npm run dev      # Vite dev server (localhost:5173)
npm run build    # Production build to dist/
npm run preview  # Preview production build
```

### Deployment

```bash
cd website
npm run build
npx wrangler pages deploy dist --project-name=slag-dev
```

Target: Cloudflare Pages at slag.dev (`wrangler.toml` configures `slag-dev` and `pages_build_output_dir = "dist"`).

`website/public/install.sh` is a copy of `slag-rs/install.sh`. Vite copies `public/` into `dist/`, which is what serves `https://slag.dev/install.sh`, the documented install path. Edit one and copy it to the other in the same commit: a deploy from a tree missing the copy takes the live installer down.

### Source Structure

- `index.html`: Entry point with terminal-UI layout and the fire header
- `src/main.js`: App init, renders ingots, wires copy buttons
- `src/content.js`: Example ingot data, `renderIngots()`, `toSExpression()` conversion
- `src/main.css`: Terminal-UI dark theme, `--slag-*` palette, mobile-first responsive (640px/1024px breakpoints)
- `public/install.sh`: Copy of `slag-rs/install.sh`, served at `https://slag.dev/install.sh`
- `public/slag.sh`: The legacy v1 script, still served for old installs

### Stack

- Vanilla JS (ES modules, no framework)
- Vite 7.x bundler
- CSS custom properties for theming
- Cloudflare Pages + Wrangler for deployment

The TUI palette in `slag-rs/src/tui.rs` (COLD, WARM, HOT, BRIGHT, PURE) mirrors the `--slag-*` variables in `website/src/main.css`. Change one, change the other.

## Legacy: slag.sh

`slag.sh` at the repo root is the v1 orchestrator. It shells out to the Claude CLI with `claude --dangerously-skip-permissions -p` and needs `claude` on PATH. It is superseded by the Rust binary and gets no new features. Do not port fixes into it and do not point new documentation at it.
