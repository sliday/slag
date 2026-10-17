# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**slag** is a Rust task orchestrator that uses metallurgical metaphors to manage AI-powered development workflows. It breaks requirements into S-expression "ingots" and forges them through an agent loop with automatic retry and proof-based verification.

slag uses OpenAI-compatible chat completions. OpenRouter stays the default. Cloud providers use their own keys; Ollama and LM Studio need no key. The engine needs no Claude CLI or vendor SDK.

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
| `engine/` | Chat-completions provider, agent loop, tools, shell sandbox, judge, recipes, events, MCP client |
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

Select the default provider with `--provider NAME`, `SLAG_PROVIDER`, or `provider = "NAME"` in `config.toml`. Precedence is flag, then environment, then file, then `openrouter`.

Presets supply these API roots, key variables, and default models:

| Provider | Base URL | Key environment variable | Default model |
|----------|----------|--------------------------|---------------|
| openrouter | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` | `openrouter/auto` |
| openai | `https://api.openai.com/v1` | `OPENAI_API_KEY` | `gpt-4.1-mini` |
| anthropic | `https://api.anthropic.com/v1` | `ANTHROPIC_API_KEY` | `claude-sonnet-4-5` |
| gemini | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` | `gemini-flash-latest` |
| groq | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` | `openai/gpt-oss-120b` |
| cerebras | `https://api.cerebras.ai/v1` | `CEREBRAS_API_KEY` | `qwen-3.8-27b` |
| xai | `https://api.x.ai/v1` | `XAI_API_KEY` | `grok-4.3` |
| mistral | `https://api.mistral.ai/v1` | `MISTRAL_API_KEY` | `mistral-small-latest` |
| deepseek | `https://api.deepseek.com` | `DEEPSEEK_API_KEY` | `deepseek-flash` |
| together | `https://api.together.ai/v1` | `TOGETHER_API_KEY` | `meta-llama/Llama-3.3-70B-Instruct-Turbo` |
| fireworks | `https://api.fireworks.ai/inference/v1` | `FIREWORKS_API_KEY` | `accounts/fireworks/models/glm-5p3-flash` |
| ollama | `http://localhost:11434/v1` | None | `qwen3:8b` |
| lmstudio | `http://localhost:1234/v1` | None | `local-model` |
| custom | Set `base_url` | `CUSTOM_API_KEY` or `SLAG_API_KEY`, optional | `default` |

Use a loaded model ID for LM Studio or custom servers. Install the Ollama model before use. Anthropic's preset uses its evaluation compatibility layer, which ignores strict tool schemas and hoists system messages.

Override any API root with `SLAG_BASE_URL` or `base_url`. Keep `/chat/completions` off the root. `SLAG_OPENROUTER_BASE` overrides OpenRouter only. Provider-specific overrides use `SLAG_<PROVIDER>_BASE_URL` or `<provider>_base_url`.

Endpoint precedence is provider environment, generic environment, OpenRouter alias, provider file, generic file, then preset. Provider-specific overrides let mixed roles use separate proxies. slag rejects foreign preset hosts before authentication and does not follow HTTP redirects.

`slag key` shows the active provider and key presence without key values. It prompts for a missing required key on a terminal. `slag key --provider NAME KEY` stores a named provider's key. Only OpenRouter uses the OpenRouter key-check endpoint.

Storage stays `~/.config/slag/config.toml` at mode 0600, or `$SLAG_CONFIG_DIR/config.toml`. Each provider uses `<provider>_api_key`. Its standard environment key takes precedence over the stored key. Local providers need no key; custom keys stay optional.

Per-role model overrides resolve flag, then environment, then file, then the provider's default:

| Role | Env | `config.toml` key | Flag |
|------|-----|-------------------|------|
| work | `SLAG_MODEL_BASE` | `model_base` | `--model` |
| plan | `SLAG_MODEL_PLAN` | `model_plan` | `--plan-model` |
| duel cast B | `SLAG_MODEL_ALT` | `model_alt` | |
| judge | `SLAG_MODEL_JUDGE` | `model_judge` | `--judge-model` |

Use `provider::model` to route one role through another provider, such as `model_plan = "openai::gpt-4.1-mini"`. Bare model IDs use the default provider. Existing OpenRouter IDs such as `openai/gpt-5` keep their meaning. Set the key for each provider you name.

`--auto` resets roles to the selected provider's default. Explicit model flags take precedence. File keys stay lowercase; environment names stay uppercase. Bare `openrouter/auto` also uses the selected provider's default. Use `openrouter::openrouter/auto` for OpenRouter routing.

OpenRouter keeps its reasoning object, `reasoning_details` replay, attribution headers, pricing lookup, and native fallback routing. slag preserves native DeepSeek `reasoning_content` and Gemini tool signatures across tool calls. The agent recovers standalone JSON against declared tool schemas and rejects prose or code fences. It returns provider tool-validation errors to the model, with a limit of three consecutive failures.

Other providers receive supported reasoning fields or omit them. `slag cost` shows session spend and a one-line balance note for non-OpenRouter providers. Spend caps depend on reported response costs outside OpenRouter.

### MCP servers

`parse_config_lines` tracks `[section]` headers and namespaces the keys under them, so an `[mcp]` table holds one stdio server per line:

```toml
[mcp]
filesystem = "npx -y @modelcontextprotocol/server-filesystem /tmp"
github = "gh-mcp --stdio"
```

`engine/mcp.rs` spawns each server once per process at forge start, handshakes it (`initialize` → `notifications/initialized` → `tools/list`), and re-exports its tools as `mcp__<server>__<tool>` beside the native eight. `ToolBox::all_specs()` is natives plus MCP; `ToolBox::specs()` stays natives-only. A server that will not spawn or handshake inside 15s is named in a warning and skipped, never fatal. Recipes gate on these names through `requires_tools` unchanged. Scope is stdio only: no HTTP, no SSE, no OAuth.

### Shell sandbox

`engine/sandbox.rs` confines the `bash` tool's writes to the workspace, `/tmp` and this user's `$TMPDIR`, the data-only cache subdirs in `HOME_CACHES` and `CACHE_ENV` (never `~/.cargo/bin`, `~/.rustup` or the npx cache), and an out-of-root git dir. The git control paths in `GIT_CONTROL` (hooks, config, `config.worktree`, `commondir`) and the `.git` entry stay read-only inside those trees, for the shell and for `write_file`/`edit_file`. Every git command slag runs itself goes through `sandbox::git()` / `git_async()`, which add `-c core.hooksPath=/dev/null -c core.fsmonitor=false`; never call `Command::new("git")` directly. `sandbox::protect_git_dir` locks slag's private repos (the `slag fix` shadow). Landlock cannot lock a file inside a writable tree, so `git_guard` records the program-running config keys when a sandbox first covers a repo and `git()` refuses once they change. Reads and network stay open. `ToolBox` resolves it at the first shell call; `bash`, `grep`, `glob`, and background jobs all run under it. `proof::run_shell_in` runs proofs and CMD lines (forge and duel) in it, and `fix::repo::run` runs every `slag fix` test command in it, because the model writes or can edit both. Hooks and `screenshot_cmd` stay outside: the user wrote them.

| Setting | Env | `config.toml` key | Flag |
|---------|-----|-------------------|------|
| mode: `auto` (default) or `off` | `SLAG_SANDBOX` | `sandbox` | `--no-sandbox` |
| extra writable paths | | `sandbox_writable = ["~/data"]` | |

Backends: macOS runs `sandbox-exec` with a generated Seatbelt profile. Linux uses `bwrap` when it works, else Landlock (raw syscalls, no crate), else runs unconfined. `main.rs` calls `sandbox::preflight()` before the dashboard starts, which prints one warning per run when nothing can confine writes. A refused write adds `write outside the workspace was blocked by slag's sandbox: <path>` to the tool result, leads a failed proof's output, and rides on a fix test run's `status()`.

Other environment knobs include `SLAG_DUEL`, `SLAG_DUEL_ROUNDS`, `SLAG_REASONING_EFFORT`, `SLAG_SCREENSHOT_CMD`, `SLAG_CONFIG_DIR`, and `SLAG_CHAR_BUDGET` (agent compaction budget). `tui.rs` also reads `NO_COLOR` and `COLORTERM`.

Behavior constants in `config.rs`: `MAX_ANVILS = 3` (parallel ingot slots), `HIGH_GRADE = 3` (plan-model threshold), `MAX_ITERATE = 3` (self-iteration rounds).

`DuelMode::Auto` duels grade 3 and up, and only when `model_alt` differs from `model_base`. With everything on `openrouter/auto` that gate stays shut, so the default never pays 3x tokens to duel a model against itself.

### Commands

```bash
cd slag-rs
cargo test              # keep this green
cargo build --release
```

Binary surface: `slag "commission"`, `slag key [KEY]`, `slag status`, `slag resume`, `slag update`. Flags: `--tui`, `--anvils N`, `--provider`, `--auto`, `--model`, `--plan-model`, `--judge-model`, `--no-sandbox`, `--worktree` (parses, warns, not implemented).

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
