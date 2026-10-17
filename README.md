# slag

[![Release](https://img.shields.io/github/v/release/sliday/slag?label=release&color=ff9940)](https://github.com/sliday/slag/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-ffd866.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-e06c75.svg)](slag-rs/)
[![LLM: OpenAI compatible](https://img.shields.io/badge/LLM-OpenAI%20compatible-6b7385.svg)](#providers-and-keys)

**Smelt ideas, skim the bugs, forge the product.**

A task orchestrator for AI-powered development. Give slag a product requirement. It splits the work into tasks, calls your chosen model, and checks each task with a shell command.

Choose any OpenAI-compatible chat-completions provider. OpenRouter stays the default. Local providers work without a key. You do not need the Claude CLI or a vendor SDK.

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

Use an environment key to skip `slag key`:

```bash
export OPENROUTER_API_KEY=sk-or-...
slag "Build a CLI tool that converts CSV to JSON"
```

Run `slag` without an argument to forge an existing `PRD.md`.

## Providers and keys

Choose the provider with `--provider NAME`, `SLAG_PROVIDER`, or `provider = "NAME"` in `config.toml`. Precedence is flag, then environment, then file, then `openrouter`.

```bash
slag --provider cerebras "Build a CLI tool"
slag --provider ollama --model qwen3:8b "Build a local notes app"
```

Each preset supplies a base URL and a default model for each role:

| Provider | Base URL | Key environment variable | Default model |
|----------|----------|--------------------------|---------------|
| [openrouter](https://openrouter.ai/docs/quickstart) | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` | `openrouter/auto` |
| [openai](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create) | `https://api.openai.com/v1` | `OPENAI_API_KEY` | `gpt-4.1-mini` |
| [anthropic](https://platform.claude.com/docs/en/cli-sdks-libraries/libraries/openai-sdk) | `https://api.anthropic.com/v1` | `ANTHROPIC_API_KEY` | `claude-sonnet-4-5` |
| [gemini](https://ai.google.dev/gemini-api/docs/openai) | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` | `gemini-flash-latest` |
| [groq](https://console.groq.com/docs/overview) | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` | `openai/gpt-oss-120b` |
| [cerebras](https://inference-docs.cerebras.ai/resources/openai) | `https://api.cerebras.ai/v1` | `CEREBRAS_API_KEY` | `qwen-3.8-27b` |
| [xai](https://docs.x.ai/developers/model-capabilities/text/generate-text) | `https://api.x.ai/v1` | `XAI_API_KEY` | `grok-4.3` |
| [mistral](https://docs.mistral.ai/api) | `https://api.mistral.ai/v1` | `MISTRAL_API_KEY` | `mistral-small-latest` |
| [deepseek](https://api-docs.deepseek.com/) | `https://api.deepseek.com` | `DEEPSEEK_API_KEY` | `deepseek-flash` |
| [together](https://docs.together.ai/docs/inference/openai-compatibility) | `https://api.together.ai/v1` | `TOGETHER_API_KEY` | `meta-llama/Llama-3.3-70B-Instruct-Turbo` |
| [fireworks](https://docs.fireworks.ai/tools-sdks/openai-compatibility) | `https://api.fireworks.ai/inference/v1` | `FIREWORKS_API_KEY` | `accounts/fireworks/models/glm-5p3-flash` |
| [ollama](https://docs.ollama.com/api/openai-compatibility) | `http://localhost:11434/v1` | None | `qwen3:8b` |
| [lmstudio](https://lmstudio.ai/docs/developer/openai-compat) | `http://localhost:1234/v1` | None | `local-model` |
| custom | Set `base_url` | `CUSTOM_API_KEY` or `SLAG_API_KEY`, optional | `default` |

Install the Ollama model before use. For LM Studio and custom endpoints, set `--model` to a model your server exposes.

Anthropic supplies an OpenAI compatibility layer for evaluation. It ignores strict tool schemas and hoists system messages. See its linked compatibility guide for limits.

| Command | What it does |
|---------|--------------|
| `slag key` | Shows the active provider and key presence. Prompts when a required key is absent on a terminal. |
| `slag key --provider openai` | Shows OpenAI setup without a key value. |
| `slag key --provider openai KEY` | Stores an OpenAI key. |

Use the prompt to enter a key. A key argument stays in your shell history. slag never prints a key value.

slag checks OpenRouter keys before storage. It saves other providers' keys without an OpenRouter check. An unreachable OpenRouter check causes a warning, then storage.

slag stores keys in `~/.config/slag/config.toml` with mode 0600. Set `SLAG_CONFIG_DIR` to move this directory. Each provider uses `<provider>_api_key`, such as `openai_api_key`. Its environment key takes precedence over the stored key.

Override a preset's endpoint with `SLAG_BASE_URL` or `base_url` in the file. Supply the API root, such as `https://example.com/v1`, without `/chat/completions`. `SLAG_OPENROUTER_BASE` overrides OpenRouter only.

For separate endpoints in a mixed run, use `SLAG_<PROVIDER>_BASE_URL` or `<provider>_base_url`. Endpoint precedence is provider environment, generic environment, OpenRouter alias, provider file, generic file, then preset. slag rejects foreign preset hosts before authentication and does not follow HTTP redirects.

`slag cost` shows session spend. For OpenRouter, it also checks the account balance. Other providers get session spend and a note about the absent balance endpoint.

Local list prices cover cloud defaults and common models, dated 2026-10-15. Sources and rates live in `slag-rs/src/engine/pricing.rs`. These estimates appear as `≈ $0.0031 est.` across spend displays. Unknown prices show input and output tokens. Mixed totals stay unknown if any call has an unknown price. Local and custom models stay unknown unless you set a price. No pricing lookup contacts another provider.

Override list prices in USD per million tokens:

```toml
[pricing]
"openai::gpt-4.1-mini" = { input = 0.40, output = 1.60, cached_input = 0.10 }
"custom::my-model" = { input = 0.20, output = 0.80 }
```

`input` and `output` are required. `cached_input` is optional. Overrides take precedence over list prices and reported costs. Spend caps use reported costs or these estimates. DeepSeek uses peak rates. Gemini Flash latest uses its current promotional rates. Estimates exclude cache storage, taxes, and discounts.

## Models

Each role uses its selected provider's default unless you set a model:

| Role | Used for | Env var | `config.toml` key | Flag |
|------|----------|---------|-------------------|------|
| work | Grade 1-2 ingots | `SLAG_MODEL_BASE` | `model_base` | `--model` |
| plan | Grade 3+ ingots, surveyor, founder | `SLAG_MODEL_PLAN` | `model_plan` | `--plan-model` |
| duel | Cast B in a twin-cast duel | `SLAG_MODEL_ALT` | `model_alt` | |
| judge | Assays duel casts | `SLAG_MODEL_JUDGE` | `model_judge` | `--judge-model` |

Model precedence is flag, then environment, then file, then the provider default. Use lowercase keys in the file and uppercase names in the environment.

Use `provider::model` to select another provider for one role. The `::` separator preserves existing OpenRouter IDs, including `openai/gpt-5`.

```bash
slag --provider cerebras --plan-model openai::gpt-4.1-mini "Build a Slack bot"
```

```toml
provider = "cerebras"
model_base = "gpt-oss-120b"
model_plan = "openai::gpt-4.1-mini"
model_judge = "openrouter::openai/gpt-5"
```

Set a key for each provider you name. A bare model ID uses the selected provider. With OpenRouter selected, `openai/gpt-5` still names an OpenRouter model.

With another provider selected, bare `openrouter/auto` uses that provider's default. Use `openrouter::openrouter/auto` to request OpenRouter routing.

Provider settings join the existing model, duel, screenshot, MCP, and spend settings in the same file. OpenRouter keeps its routing, `reasoning_details` replay, pricing lookup, and attribution headers. slag preserves native DeepSeek `reasoning_content` and Gemini tool signatures across tool calls. The agent recovers standalone JSON against declared tool schemas and rejects prose or code fences. It returns provider tool-validation errors to the model, with a limit of three consecutive failures. Other providers receive supported reasoning fields or omit them.

### MCP servers

An `[mcp]` table adds Model Context Protocol servers over stdio, one command per line:

```toml
[mcp]
filesystem = "npx -y @modelcontextprotocol/server-filesystem /tmp"
github = "gh-mcp --stdio"
```

slag spawns each server at forge start and hands the smith every tool it advertises, named `mcp__<server>__<tool>`, beside the built-in eight. A server that will not start is named in a warning and skipped; the forge runs on the natives. stdio transport only, so no HTTP, SSE, or OAuth servers.

`--auto` resets all four roles to the selected provider's default. Explicit model flags take precedence.

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
3. **Strike**, running an agent loop against the selected provider with the task description
4. **Proof**, running the `:proof` command
5. Pass: mark `:forged`, git commit. Fail: increment `:heat`, retry with the error fed back. Max retries: mark `:cracked`.

Independent ingots (`:solo t`) run on up to 3 parallel anvils. Dependent ingots (`:solo nil`) run sequentially after.

High-grade ingots can go to a twin-cast duel: two casts build the same ingot in separate git worktrees, a judge model assays both, and the winner merges. Duels only fire when cast B runs a different model from the worker, since two rolls of the same dice cost triple the tokens and buy nothing. Set `SLAG_MODEL_ALT` to a different family to turn duels on for grade 3 and up, or force the behavior with `SLAG_DUEL=on` / `off`.

### Phase 4: ASSAY

Final report. Counts forged versus cracked ingots, shows a temperature bar, and exits 0 on a full forge or 1 if anything cracked. It prints on failed runs too, since a run that cracked an ingot is when you most need the cracked list.

## Fixing an issue: `slag fix`

The pipeline above builds a project. `slag fix` solves one issue in a repository that already has code and tests, and skips the survey, the plan, and every file they write:

```bash
slag fix "export drops the last day of the range"
slag fix 123                      # GitHub issue in this repo, fetched with gh
slag fix https://github.com/owner/repo/issues/123
slag fix --issue-file ISSUE.md --pr
```

1. slag finds the test command (package.json, Cargo.toml, go.mod, Python tests, Makefile, or `--test CMD`) and runs it before any edit.
2. One agent session reads the code, adds a regression test, fixes the bug, and runs the tests.
3. slag reruns the tests itself. The verdict comes from those exit codes, not from the model's summary. A session that deletes or skips an existing test fails.
4. slag reverts the source change and reruns the tests to show the new test catches the bug, then puts the fix back.
5. The fix lands as one commit on a new `slag/fix-<slug>` branch (`slag/fix-<n>-<slug>` for a GitHub issue). The report prints the diff stat and the verification command.

A failed attempt retries on the plan model, starting from the failure slag saw. `--tries N` sets the attempts (default 3), `--no-commit` leaves the fix in the working tree, `--pr` pushes the branch and opens a draft PR, and `--temper` adds the warden as an independent judge. Outside git, slag tracks the changes in a private temporary repository and commits nothing.

## Sandbox

The smith's shell commands can read anything and reach the network. They can write only inside the workspace, `/tmp` and your own `$TMPDIR`, and the data-only tool caches: `~/.cargo/registry`, `~/.cargo/git`, `~/.npm/_cacache` and `_logs`, `pip`, `uv`, `go-build` and Yarn caches under `~/.cache` or `~/Library/Caches`, the pnpm, bun and Yarn Berry stores, `~/.gradle/caches`, `~/.m2/repository`, and `~/go/pkg/mod`. `CARGO_HOME`, `GOPATH` and `XDG_CACHE_HOME` move those caches; `CARGO_TARGET_DIR`, `GOCACHE` and `GOMODCACHE` are writable whole. Nothing on your `PATH` or loaded as a program is writable: not `~/.cargo/bin`, `~/.rustup`, the npx cache, global package bins, the rest of `~/Library/Caches`, or other users' temp dirs. If a tool needs more, add it with `sandbox_writable`.

Nothing a sandboxed command writes may become code that runs outside the sandbox. slag's own git (commits, snapshots, diffs, worktrees) runs outside it, so the workspace's `.git/hooks`, `.git/config`, `config.worktree`, `commondir` and the `.git` entry itself stay read-only for model commands and for the file tools, and every git command slag runs passes `-c core.hooksPath=/dev/null -c core.fsmonitor=false`. Ordinary git use that writes objects, refs and the index keeps working; `git init` over the workspace and `git config` do not. Landlock cannot carve a read-only file out of a writable tree, so on Landlock slag instead records what the git config would run and refuses its own git once a command changes that. A workspace in a linked git worktree or a repo subdirectory can also write its git dir's objects and refs. The file tools already refuse paths outside the workspace. The sandbox needs no setup:

| Platform | Backend |
|----------|---------|
| macOS | `sandbox-exec` with a generated Seatbelt profile |
| Linux with `bwrap` | bubblewrap: the whole filesystem read-only, the writable paths bound read-write |
| Linux without `bwrap` | Landlock, kernel 5.13 or later |
| Linux with neither | No confinement. slag prints one warning per run that names bubblewrap. |
| Other platforms | No confinement, with one warning per run |

On macOS, `sandbox-exec` fails when slag already runs inside another sandbox. slag then runs unconfined and warns once. On Linux, a `bwrap` that fails to start (no user namespaces) falls through to Landlock.

When the sandbox refuses a write, the tool result tells the model `write outside the workspace was blocked by slag's sandbox: <path>` (or `write to git's hooks or config was blocked by slag's sandbox: <path>`), so it changes approach instead of retrying.

```toml
sandbox = "auto"                               # or "off"
sandbox_writable = ["~/data", "/srv/scratch"]  # extra writable paths
```

`--no-sandbox` or `SLAG_SANDBOX=off` turns it off. Precedence is flag, then environment, then file. A model writes every `:proof` and `CMD:` line, and `slag fix` can edit the test scripts it then runs, so slag runs proofs, CMD checks, and `slag fix` test runs in the same sandbox. A refused write shows up in the failure as `write blocked by slag's sandbox: <path>`. Hooks and `screenshot_cmd` stay outside it because you wrote them.

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
          OpenAI-compatible API (OpenRouter by default)
```

The engine uses its own agent loop and OpenAI-compatible chat completions. It needs no vendor CLI.

## Commands

| Command | Role |
|---------|------|
| `slag "commission"` | Write `PRD.md` and run the full pipeline |
| `slag` | Forge the `PRD.md` already in this directory |
| `slag fix ISSUE` | Fix one issue in this repository and commit it on a new branch |
| `slag key [KEY] [--provider NAME]` | Show provider setup or store its key |
| `slag cost` | Show session spend and an OpenRouter balance when available |
| `slag status` | Show crucible state |
| `slag resume` | Resume an existing forge |
| `slag update` | Self-update to the latest release |

## Flags

| Flag | Effect |
|------|--------|
| `--tui` | Full-screen dashboard with crucible, live feed, and steering input |
| `--anvils N` | Max parallel anvil workers (default 3) |
| `--provider NAME` | Select the default provider |
| `--auto` | Reset all four roles to the selected provider default |
| `--model MODEL` | Worker model |
| `--plan-model MODEL` | Planner model for grade 3+ ingots |
| `--judge-model MODEL` | Duel judge model |
| `--worktree` | Branch-per-ingot isolation. Not implemented yet, warns and runs in the shared checkout. |
| `--no-sandbox` | Let shell commands write outside the workspace (see [Sandbox](#sandbox)) |

## Environment

| Variable | Effect |
|----------|--------|
| Provider key from the table above | Overrides that provider's stored key |
| `SLAG_PROVIDER` | Default provider; `--provider` takes precedence |
| `SLAG_BASE_URL` | Override the selected provider's API root |
| `SLAG_<PROVIDER>_BASE_URL` | Override one provider's API root in a mixed run |
| `SLAG_MODEL_BASE` / `_PLAN` / `_ALT` / `_JUDGE` | Per-role model ids |
| `SLAG_DUEL` | `auto` (default), `on`, `off` |
| `SLAG_DUEL_ROUNDS` | Override the duel round cap |
| `SLAG_SANDBOX` | `auto` (default) or `off` for the shell sandbox |
| `SLAG_MAX_COST_INGOT` | Dollar ceiling for a single ingot session (file key `max_cost_per_ingot`). Default uncapped. |
| `SLAG_MAX_COST_RUN` | Dollar ceiling for the whole run (file key `max_cost_per_run`). Default uncapped. |
| `SLAG_REASONING_EFFORT` | `low`, `medium`, `high` for models that take it |
| `SLAG_SCREENSHOT_CMD` | Shell command producing a screenshot for visual assay of web ingots |
| `SLAG_CONFIG_DIR` | Move the config directory off `~/.config/slag` |
| `SLAG_OPENROUTER_BASE` | Legacy OpenRouter-only base URL override |
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

- A key for your provider, or a local Ollama or LM Studio server
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
