# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**slag** is a Rust task orchestrator that uses metallurgical metaphors to manage AI-powered development workflows. It breaks requirements into S-expression "ingots" and forges them through an agent loop with automatic retry and proof-based verification.

slag speaks OpenAI-compatible chat completions, and `/responses` for OpenAI GPT-5.4 and later. OpenRouter stays the default. Cloud providers use their own keys; Ollama and LM Studio need no key. The engine needs no Claude CLI or vendor SDK.

The repo has three layers:

1. **`slag-rs/`**: The orchestrator, a Rust binary shipped through GitHub Releases
2. **`website/`**: The Vite site for slag.dev, deployed to Cloudflare Pages
3. **`slag.sh`**: The superseded v1 bash orchestrator, kept for reference only (see Legacy below)

## Orchestrator Architecture (slag-rs/)

### Pipeline: four phases plus temper

1. **SURVEYOR**: Reads `.slag/PRD.md` (ore), produces `.slag/BLUEPRINT.md` on the plan model
2. **FOUNDER**: Reads blueprint, generates S-expression ingots in `.slag/PLAN.md`
3. **FORGE**: Picks ore, runs an agent session with tools, verifies with `:proof`, commits on pass
4. **ASSAY**: Final report with pass/fail status. `pipeline::run` calls it on every outcome except `Cancelled`, so a cracked run still prints its counts.

`--temper N` arms the warden (`pipeline/warden.rs`), a fresh critic on the plan model with tools and the strict read-only sandbox. `warden::bar` derives `.slag/BAR.md` once. `checkpoint` judges the blueprint and the plan before the forge, with one corrective pass each. `judge_ingot` judges an ingot with a non-empty `:bar` after its proof passes; a loss burns the heat. After the forge, `judge_artifact` judges the built project; a loss becomes an addendum, `founder::extend` casts gap ingots, and the forge runs again, up to N rounds. An unmet goal ends in `SlagError::GoalNotMet` (exit 1). `warden/checkout.rs` runs the suite from a fresh clone; `warden/grounding.rs` ties a verdict to the commands the warden ran.

With temper armed, a commission of 800 chars or fewer in an empty directory can take the direct path: `surveyor::direct_proof` asks `pipeline/direct.rs` for one contract, and `found_direct` writes one ingot, BLUEPRINT.md and BAR.md, with fixtures only under `.slag/checks/`. `pipeline/hygiene.rs` writes the ignore rules (`.git/info/exclude` for slag's files, `.gitignore` for language caches).

### Ingot S-Expression Format

```
(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f file" :work "Task description")
```

`sexp/parser.rs` parses thirteen fields and preserves anything else in `Ingot::extra`:

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
| `:bar` | string | The ingot's acceptance bar. With temper armed, `warden::judge_ingot` judges it after the proof; empty means proof alone |
| `:work` | string | Task description for the smith |
| `:duel` | t / nil | Twin-cast override: t forces, nil blocks, absent defers to `DuelMode` |
| `:casts` | 1 / 2 / 3 | Cast-count override; absent defers to the `DuelMode` heuristics |

### Source Layout (slag-rs/src)

| Path | Role |
|------|------|
| `main.rs` | Entry point, subcommand dispatch, key gate before any forge |
| `cli.rs` | clap definitions for flags and subcommands; `status --json`, `runs`, `ps`, `rewind`, `cost`, `hooks list` |
| `config.rs` | `EngineConfig`, key storage and verification, model resolution, `.slag/` layout (`project_path`), spend caps, hook entries |
| `engine/` | Provider (chat completions and Responses), agent loop, tools, shell sandbox, hooks, command policy, pricing, judge, recipes, events, compaction, transcripts, MCP client |
| `smith/` | `NativeSmith` factories (`make_smith`, `make_plan_smith`, `make_base_smith`) |
| `pipeline/` | surveyor, founder, forge, duel, resmelt, assay, warden (`--temper`), direct (the short-commission path), hygiene (ignore rules), fix (`fix/evolve.rs` is `--evolve`) |
| `harness/` | The execution harness (`mod.rs`), its validator, the library, and the improver's stage prompts |
| `anvil/` | Worktree helpers and attempt checkpoints (`checkpoint.rs`, read by `slag rewind`). The parallel fan-out itself lives in `pipeline/forge.rs` |
| `crucible.rs`, `sexp/` | Ingot parsing and crucible state |
| `flux.rs`, `proof.rs` | Prompt builders; `:proof` and CMD runners |
| `tui.rs`, `dashboard.rs` | Stream-mode output and the Ratatui dashboard |
| `render/` | Presentation shared by the stream view and the dashboard: stream, echo, diff, the tape (`tape.rs`) and `--trace` export (`trace.rs`) |
| `replay.rs` | `slag replay`: plays a tape through the live renderers |
| `insights.rs` | `slag insights`: offline analytics over the logs, cached in `logs/facets/` |
| `migrations.rs` | Idempotent startup fixups before any command runs |
| `steer_history.rs`, `shutdown.rs` | Steer history in `~/.slag/history.jsonl`; the cleanup registry for every exit path |
| `update.rs` | Self-update from GitHub Releases (`sliday/slag`) |

### Project Files the Pipeline Reads and Writes

New commissions use gitignored `.slag/`. Slag stores its ignore patterns in `.git/info/exclude`. Language cache rules stay in `.gitignore`. Projects with root `PLAN.md` or `PRD.md` keep the root layout. The `.slag/layout` marker pins the new layout.

| File | Role |
|------|------|
| `.slag/PRD.md` | Requirements input (ore) |
| `.slag/BLUEPRINT.md` | Surveyor analysis output |
| `.slag/PLAN.md` | Ingot crucible (task list) |
| `.slag/AGENTS.md` | Coordinator scratch notes. The coordinator creates a `## Alloy Recipes` stub. Forge prompts read this file. |
| `.slag/BAR.md` | Acceptance criteria and required checks |
| `.slag/PROGRESS.md` | Work history ledger |
| `.slag/logs/` | Debug logs (slag heap): `engine-*.jsonl`, `run-*.jsonl`, `tape-*.jsonl`, `checkpoints/`, `transcripts/`, `facets/` |
| `.slag/layout` | Marker that pins the `.slag/` layout (`config::project_path`) |
| `.slag/session-costs.json` | Spend per run id (`dashboard.rs`), read by `slag cost` and `slag status --json` |
| `.slag/evolve/<run>/` | `--evolve` record (`record.json`) and improver session roots; `.slag/evolve/latest.json` copies the last record |
| `.slag/library/` | Optional per-repository harness library |
| `.slag/checks/` | Untracked verification probes and reference fixtures. Builders can write here; reviewers cannot. Product tests must not depend on it. |
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
| editor (`slag fix --evolve`; default: plan) | `SLAG_MODEL_EDITOR` | `model_editor` | `--editor-model` |

Use `provider::model` to route one role through another provider, such as `model_plan = "openai::gpt-4.1-mini"`. Bare model IDs use the default provider. Existing OpenRouter IDs such as `openai/gpt-5` keep their meaning. Set the key for each provider you name.

`--auto` resets roles to the selected provider's default. Explicit model flags take precedence. File keys stay lowercase; environment names stay uppercase. Bare `openrouter/auto` also uses the selected provider's default. Use `openrouter::openrouter/auto` for OpenRouter routing.

OpenRouter keeps its reasoning object, `reasoning_details` replay, attribution headers, pricing lookup, and native fallback routing. slag preserves native DeepSeek `reasoning_content` and Gemini tool signatures across tool calls. The agent recovers standalone JSON against declared tool schemas and rejects prose or code fences. It returns provider tool-validation errors to the model, with a limit of three consecutive failures.

OpenAI models from GPT-5.4 onward use `/responses`. Older models keep chat completions. A matching tools/reasoning rejection switches once per model and endpoint. slag replays encrypted reasoning and output items without server-side storage.

Other providers receive supported reasoning fields or omit them. `slag cost` shows session spend and a one-line balance note for non-OpenRouter providers.

`engine/pricing.rs` holds cloud list prices and source URLs, dated per entry. OpenAI rates include GPT-6.1 Sol and GPT-5.5, checked 2026-10-19. It estimates default and common models without network calls.

All spend displays use `pricing::format_cost`: `≈ $0.0031 est.` for estimates, or input/output tokens when the price is unknown. Mixed totals stay unknown if any call lacks a price. Only OpenRouter reads its `pricing.json` catalog cache. Local/custom models need an override for a cost estimate.

```toml
[pricing]
"openai::gpt-4.1-mini" = { input = 0.40, output = 1.60, cached_input = 0.10 }
```

Rates use USD per million tokens. Input and output rates are required; cached input is optional. Overrides win over list prices and reported costs. Spend caps use reported costs or estimates. DeepSeek uses peak rates; Gemini Flash latest uses current promotional rates. Estimates exclude cache storage, taxes, and discounts.

### MCP servers

`parse_config_lines` tracks `[section]` headers and namespaces the keys under them, so an `[mcp]` table holds one stdio server per line:

```toml
[mcp]
filesystem = "npx -y @modelcontextprotocol/server-filesystem /tmp"
github = "gh-mcp --stdio"
```

`engine/mcp.rs` spawns each server once per process at forge start, handshakes it (`initialize` → `notifications/initialized` → `tools/list`), and re-exports its tools as `mcp__<server>__<tool>` beside the native eight. `ToolBox::all_specs()` is natives plus MCP; `ToolBox::specs()` stays natives-only. A server that will not spawn or handshake inside 15s is named in a warning and skipped, never fatal. Recipes gate on these names through `requires_tools` unchanged. Scope is stdio only: no HTTP, no SSE, no OAuth.

### Shell sandbox

`engine/sandbox.rs` confines the `bash` tool's writes to the workspace, `/tmp` and this user's `$TMPDIR`, `~/.npm/_logs`, and an out-of-root git dir. No package cache is writable (each holds code a later outside build runs; cargo does not re-check a cached `.crate`); `THROWAWAY_CACHES` points npm, pnpm, Yarn, bun, pip, uv and `GOCACHE` at `$TMPDIR/slag-cache-$USER` for sandboxed commands, and an outside `CARGO_TARGET_DIR` becomes `<root>/target`. A refused write into a package cache gets `CACHE_TAIL` (fetch outside, vendor, or `sandbox_writable`). The git control paths in `GIT_CONTROL` (hooks, config, `config.worktree`, `commondir`) and the `.git` entry stay read-only inside those trees, for the shell and for `write_file`/`edit_file`. Every git command slag runs itself goes through `sandbox::git()` / `git_async()`, which add `-c core.hooksPath=/dev/null -c core.fsmonitor=false`; never call `Command::new("git")` directly. `sandbox::protect_git_dir` locks slag's private repos (the `slag fix` shadow). Landlock cannot lock a file inside a writable tree, so `git_guard` records the program-running config keys when a sandbox first covers a repo and `git()` refuses once they change. Reads and network stay open. Read-only reviews allow writes only in private temporary scratch space and remove it after each command. Tests use TMPDIR there; Python probes run from temporary scripts. Package installation checks use a separate temporary clone and runtime. They fall back from venv to virtualenv or pip with a private target. Missing installer tools produce a recorded skip; package build failures remain errors. Strict bubblewrap checks drop capabilities, including for root. Strict reviews refuse Landlock because it cannot block metadata writes; slag warns once and does not run those checks. `ToolBox` resolves it at the first shell call; `bash`, `grep`, `glob`, and background jobs all run under it. `proof::run_shell_in` runs proofs and CMD lines (forge and duel) in it, and `fix::repo::run` runs every `slag fix` test command in it, because the model writes or can edit both. Hooks, `screenshot_cmd` and `[mcp]` servers stay outside: the user wrote them. Every other spawn is either a fixed binary with fixed arguments (`tar`, `kill`, `uname`, `gh` with `GIT_GUARD_ENV`, guarded `git`) or runs through `Sandbox::wrap`/`wrap_std`; anything whose program, script or import the workspace controls (test detection's `imports()` probe included) must take the sandbox.

| Setting | Env | `config.toml` key | Flag |
|---------|-----|-------------------|------|
| mode: `auto` (default) or `off` | `SLAG_SANDBOX` | `sandbox` | `--no-sandbox` |
| extra writable paths | | `sandbox_writable = ["~/data"]` | |

Backends: macOS runs `sandbox-exec` with a generated Seatbelt profile. Linux uses `bwrap` when it works, else Landlock (raw syscalls, no crate), else runs unconfined. `main.rs` calls `sandbox::preflight()` before the dashboard starts, which prints one warning per run when nothing can confine writes. A refused write adds `write outside the workspace was blocked by slag's sandbox: <path>` to the tool result, leads a failed proof's output, and rides on a fix test run's `status()`.

### Recursive harnessing (`slag fix --evolve`, `--harness`; experimental)

After Harness² (Xu et al., arXiv:2610.14224). `harness/mod.rs` loads a directory of files: `systemprompt.md`, `guardrails.md`, `memory.md` and `tool_guidance.md` close the stable band (`prompt::build_with`); `recipes/<name>/RECIPE.md` join the recipe index (`recipes::index_with`, `ToolBox::with_harness_recipes`); `scripts/` are named in the band and run through the sandboxed `bash`; `hooks.toml` lines become a `HookSnapshot` whose `CommandScope` runs them in the read-only sandbox, fired before the operator's hooks on the way in. Subagents have no slag equivalent. `NativeSmith::with_harness` drops an empty harness, so the fast path's prompt stays byte-identical (tested in `prompt.rs`).

`pipeline/fix/evolve.rs` runs reference, propose, probe, compose, execute and select. Each run is `Ctx::attempt` in a copy of the working tree with a shadow git, under the candidate harness. Every session shares one `agent::SharedBudget`: dollars only from calls with a cost (list price, `[pricing]` override, or OpenRouter's report, never an invented rate), every token, and the unpriced tokens apart; `ForgeAgent` checks it each turn, and the reference runs get `counting_only()` so the cap never stops the plain fix midway. Stage spend reads `≈ $x est.` plus any unpriced tokens. Improver sessions are `ForgeAgent`s with `Role::Harness`, a file-tool allowlist rooted at `.slag/evolve/<run>/sessions/<name>`, no finish tool, `with_required_reads` over their evidence, every tool round kept in view (`with_recent_rounds`), and `guard()` run first. `harness::validate` retries once, then falls back. A stage error after a run verified stops the recursion like a cap (`record.capped`), calls `error::clear_provider_failure()` because evolve now owns that error, and delivers the verified run; a cancel, or an error with nothing verified, still ends the fix. A composer that keeps edits in its notes and writes none gets them copied by `apply_kept` before any retry. `Evolved::reflect` writes a library entry only when `library::admit` passes (a backed row, a named component, evidence citing a run label); `support()` sets the entry's confidence from run counts and the deciding measure (low for one run against one, or turns and tool errors alone). `Library::apply` (`--harness library`) brings every component of the top entries and returns what each gave, which `fix.rs` puts on the done screen. `record.json` and `.slag/evolve/latest.json` feed the done screen and `slag status --json`.

| Setting | Env | `config.toml` key | Flag |
|---------|-----|-------------------|------|
| library | `SLAG_HARNESS_LIBRARY` | `harness_library` | `--library` (else `.slag/library`, else `<config dir>/library`) |
| dollar cap | `SLAG_EVOLVE_BUDGET` | `evolve_budget` | `--evolve-budget` (default 2) |
| token cap | `SLAG_EVOLVE_TOKENS` | | `--evolve-tokens` (default 4000000) |

### Hooks

`engine/hooks.rs` runs lifecycle hooks from the `[hooks]` table of `config.toml` (`config::hook_entries`), one per line, keyed by event: `pre_tool`, `post_tool`, `tool_error`, `stop`, `session_start`, `pre_compact`, `ingot_forged`, `ingot_cracked`. Kinds are `cmd=` (default), `prompt=`, `agent=` and `url=` (HTTP POST). Exit 0 turns stdout into model context, exit 2 blocks (a `pre_tool` call is refused), anything else is logged. `HookSnapshot` freezes the config at forge start. `disable_all_hooks` or `SLAG_DISABLE_HOOKS` turns all of them off. `slag hooks list` prints the contract.

### Other settings

Environment knobs not covered above: `SLAG_DUEL`, `SLAG_DUEL_ROUNDS`, `SLAG_REASONING_EFFORT`, `SLAG_SCREENSHOT_CMD`, `SLAG_CONFIG_DIR`, `SLAG_CHAR_BUDGET` (agent compaction budget in chars, default the model window less reserves, at most 800000, and 400000 when the window is unknown), `SLAG_MAX_COST_INGOT` and `SLAG_MAX_COST_RUN` (spend caps), `SLAG_CREDIT_FLOOR` (OpenRouter balance warning), `SLAG_COST_PER_MTOK` (flat rate spend caps charge for an unpriced response, default 5), `SLAG_MODEL_FALLBACK` (OpenRouter `models` fallback), `SLAG_UNATTENDED_RETRY` (retry 429/529 past the budget), `SLAG_MAX_RETRIES` (default 8), `SLAG_API_TIMEOUT_MS` (default 300000), `SLAG_BASH_OUTPUT_CAP` (default 30000), `SLAG_ALLOW_DESTRUCTIVE=1` (lift the destructive-command gate), and `SLAG_DISABLE_HOOKS`. The file-backed ones (`credit_floor`, `fallback_model`, `unattended_retry`, `max_cost_per_ingot`, `max_cost_per_run`) read env first. `tui.rs` also reads `NO_COLOR` and `COLORTERM`.

Behavior constants in `config.rs`: `MAX_ANVILS = 3` (parallel ingot slots), `HIGH_GRADE = 3` (plan-model threshold), `MAX_ITERATE = 3` (self-iteration rounds).

`DuelMode::Auto` uses work shape and grade to select one, two, or three casts for independent ingots. Grades 3-4 usually use two casts, grade 5 or taste-heavy work uses three, and mechanical work stays single-cast. `SLAG_DUEL=off` is a kill switch: one cast everywhere, beating `:casts` and `:duel` pins (only users write those; the founder prompt offers neither `:casts` nor `:duel`). Otherwise a `:casts` pin wins, then `:duel`, then `SLAG_DUEL=on`. Auto gives one cast whenever `model_alt` equals `model_base` (the default, all on `openrouter/auto`): same-model duels cost about 60% more in measured builds and changed no outcome. Set a distinct `model_alt` to open the heuristics; `on` and the pins still duel matched models.

### Commands

```bash
cd slag-rs
cargo test              # keep this green
cargo build --release
```

Binary surface: `slag "commission"`, `slag fix ISSUE` (`--issue-file`, `--test CMD`, `--tries N`, `--pr`, `--no-commit`, `--temper`, `--model`, `--plan-model`, `--tui`, `--evolve K`, `--sequential`, `--harness DIR|library|none`, `--editor-model`, `--evolve-budget`, `--evolve-tokens`, `--domain`, `--library`), `slag key [KEY]`, `slag status [--json]`, `slag runs`, `slag insights [--refresh]`, `slag ps`, `slag resume`, `slag rewind [--ingot ID --heat N]`, `slag cost`, `slag replay FILE` (`--speed`, `--tui`, `--at`, `--hold`, `--size`, `--points`), `slag hooks list`, `slag update`. Flags: `--tui`, `--temper N`, `--force`, `--anvils N`, `--provider`, `--auto`, `--model`, `--plan-model`, `--judge-model`, `--duel`, `--trace FILE`, `--no-sandbox`, `--worktree` (parses, warns, not implemented). `--provider` and `--no-sandbox` also work on every subcommand.

Releases: v3.0.0 and later ship four targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` (glibc 2.17 floor). `slag-rs/dist-local/publish-release.sh --build` builds them, and `install.sh` and `update.rs` handle all four. `slag-rs/.github/workflows/` sits below the repo root, so GitHub never runs it.

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
- `public/media/`: The hero video (`slag-hero.webm`, `slag-hero.mp4`) and its poster
- `video/`: The Remotion project that renders `public/media/` (`./render.sh`)
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
