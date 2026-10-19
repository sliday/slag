# slag

[![Release](https://img.shields.io/github/v/release/sliday/slag?label=release&color=ff9940)](https://github.com/sliday/slag/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-ffd866.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-e06c75.svg)](slag-rs/)
[![LLM: OpenAI compatible](https://img.shields.io/badge/LLM-OpenAI%20compatible-6b7385.svg)](#providers-and-keys)

**Smelt ideas, skim the bugs, forge the product.**

A task orchestrator for AI-powered development. Give slag a product requirement. It splits the work into tasks, calls your chosen model, and checks each task with a shell command.

Choose any provider with an OpenAI-compatible API. slag speaks chat completions, and the Responses API for OpenAI GPT-5.4 and later. OpenRouter stays the default. Local providers work without a key. You do not need the Claude CLI or a vendor SDK.

## Quick start

```bash
# install (single binary)
curl -sSf https://slag.dev/install.sh | sh

# one-time setup: paste your OpenRouter key
slag key

# forge
slag "Build a REST API with auth and rate limiting"
```

That third line writes `.slag/PRD.md` for you and runs the full pipeline. Get a key at [openrouter.ai/keys](https://openrouter.ai/workspaces/default/keys).

Use an environment key to skip `slag key`:

```bash
export OPENROUTER_API_KEY=sk-or-...
slag "Build a CLI tool that converts CSV to JSON"
```

Run `slag` without an argument to forge an existing commission.

New commissions keep coordination files in gitignored `.slag/`. Slag stores its ignore patterns in `.git/info/exclude`. Language cache rules stay in `.gitignore`. Existing projects with root `PLAN.md` or `PRD.md` keep their root layout.

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

OpenAI models from GPT-5.4 onward use the Responses API. Older models keep chat completions, with one cached switch after a tools/reasoning rejection. slag replays encrypted reasoning across tool calls without server-side storage.

`slag cost` shows session spend. For OpenRouter, it also checks the account balance. Other providers get session spend and a note about the absent balance endpoint.

Local list prices cover cloud defaults and common models. Each entry records its date. OpenAI rates include GPT-6.1 Sol and GPT-5.5, checked 2026-10-19.

Sources and rates live in `slag-rs/src/engine/pricing.rs`. These estimates appear as `≈ $0.0031 est.` across spend displays. Unknown prices show input and output tokens. Mixed totals stay unknown if any call has an unknown price. Local and custom models stay unknown unless you set a price. No pricing lookup contacts another provider.

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
| editor | Harness editor, composer, selector, reflector in `slag fix --evolve`. Default: the plan model | `SLAG_MODEL_EDITOR` | `model_editor` | `--editor-model` (on `slag fix`) |

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

`--auto` resets all four roles to the selected provider's default. Explicit model flags take precedence.

### MCP servers

An `[mcp]` table adds Model Context Protocol servers over stdio, one command per line:

```toml
[mcp]
filesystem = "npx -y @modelcontextprotocol/server-filesystem /tmp"
github = "gh-mcp --stdio"
```

slag spawns each server at forge start and hands the smith every tool it advertises, named `mcp__<server>__<tool>`, beside the built-in eight. A server that will not start is named in a warning and skipped; the forge runs on the natives. stdio transport only, so no HTTP, SSE, or OAuth servers.

### Hooks

A `[hooks]` table in `config.toml` runs your commands at points in a forge. Each line holds one hook, keyed by its event:

```toml
[hooks]
post_tool = "name=fmt matcher=edit_file cmd='cargo fmt'"
```

The events are `pre_tool`, `post_tool`, `tool_error`, `stop`, `session_start`, `pre_compact`, `ingot_forged` and `ingot_cracked`. The exit code is the contract. Exit 0 turns stdout into context the model sees. Exit 2 on `pre_tool` refuses the call and returns stderr as the tool result; on `post_tool` and `tool_error` it appends stderr to the result. slag logs any other code and carries on. `slag hooks list` prints the events, the contract, and your hooks. `SLAG_DISABLE_HOOKS=1` or `disable_all_hooks = "t"` turns them all off. slag reads the table once when a forge opens, so a smith cannot add a hook to its own run.

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
- **A contained shell.** The smith's shell commands write only inside the workspace and temp directories. See [Sandbox](#sandbox).

## How it works

slag runs four phases. `--temper N` adds a fifth, the warden, between the forge and the assay.

File names below refer to `.slag/` for new commissions.

```
PRD.md --> SURVEYOR --> BLUEPRINT.md --> FOUNDER --> PLAN.md --> FORGE ------> ASSAY
 (ore)    (analyze)    (analysis)      (design)   (ingots)   (strike)      (report)
                                                     ^           |
                                                     |           v  --temper N
                                                     +--- gap -- TEMPER (warden)
```

### Phase 1: SURVEYOR

The surveyor reads `.slag/PRD.md` (the ore). It writes `.slag/BLUEPRINT.md` with architecture decisions, dependencies, risks, and the forging sequence. It uses the plan model.

### Phase 2: FOUNDER

The founder reads the blueprint and casts S-expression ingots into `.slag/PLAN.md`. Each ingot holds one task:

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

Eligible independent ingots use two or three casts in separate git worktrees, with minimal, robust, or creative direction prompts. A judge model assays passing casts, then slag merges the winner. Auto mode uses work shape and grade: grades 3-4 usually use two casts, grade 5 or taste-heavy work uses three, and mechanical work stays single-cast. `:casts`, `:duel`, and `SLAG_DUEL` can override the choice. Cast B may use the same model as cast A.

### Temper: the warden (`--temper N`)

`:proof` shows that a task's artifact appeared. It does not show that the commission was met. `--temper N` adds a warden: a fresh critic on the plan model that built nothing.

1. The warden writes the acceptance bar to `.slag/BAR.md` once and judges every round against it.
2. Before the forge, it judges the blueprint and the plan against the bar. A gap sends each back for one corrective pass.
3. An ingot with a `:bar` gets judged after its proof passes. A loss counts as a failed heat.
4. After the forge, the warden opens the built project, runs its checks in a read-only sandbox, and compares the result with the bar. On a loss, the gap it names becomes new ingots, and the forge runs again, up to N rounds.

A goal still unmet after N rounds exits 1 and names the gap. Raise the count and resume to forge it, for example `slag --temper 2 resume` after one round. `0`, the default, skips the warden.

With `--temper`, a short commission (800 characters or fewer) in an empty directory can take a direct path: one ingot, one bar, one final warden. The contract checks named behavior and boundary cases first, then interfaces that fit the tool: stdin for Unix filters, positional values for scalar parsers, package entry points when requested. The reviewer runs the suite and CLI probes in a read-only sandbox, and runs the suite again from a fresh clone. Package installation checks use a separate temporary copy and try venv, virtualenv, then pip with a private target. Missing host tools produce a recorded skip; the other checks still apply. Verification instruments live in gitignored `.slag/checks/`, and product tests must work without that directory.

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

A failed attempt retries on the plan model, starting from the failure slag saw. `--tries N` sets the attempts (default 3), `--no-commit` leaves the fix in the working tree, `--pr` pushes the branch and opens a draft PR, and `--temper` adds the warden as an independent judge. `--model` and `--plan-model` pick the first-attempt and retry models, and `--tui` opens the dashboard. Outside git, slag tracks the changes in a private temporary repository and commits nothing.

## Recursive harnessing (experimental)

`slag fix --evolve K` and `--harness` are experimental. They implement Harness² (Xu et al., "Harness²: Recursive Agent Harnessing for an Open World", Google Cloud AI Research, 2026, [arXiv:2610.14224](https://arxiv.org/abs/2610.14224)): slag edits the harness it works in (prompt files, recipes, scripts, hooks), issue by issue, with no model change. `--evolve 1` runs up to four fix attempts and four improver sessions, plus validation retries, before it delivers, so expect several times the spend of a plain `slag fix`. A $2 cap applies by default. Without these flags, `slag fix` runs exactly as described above.

<!-- harness2-result -->
No measured accuracy result for `--evolve` is published yet.

```bash
slag fix "export drops the last day"                   # fast path, no harness
slag fix --harness ./my-harness "export drops ..."     # run under a harness directory
slag fix --harness library "export drops ..."          # apply the library, no probing
slag fix --evolve 1 "export drops ..."                 # evolve the harness on this issue
slag fix --evolve 3 --sequential "export drops ..."    # three chained steps instead
```

### The harness

A harness is a directory of plain files. slag loads each component through a surface it already has:

| Paper component | File | How slag loads it |
|-----------------|------|-------------------|
| C1 system prompt | `systemprompt.md` | Appended to the stable prompt band |
| C2 guardrails | `guardrails.md` | The same band |
| C3 memory | `memory.md` | The same band |
| C4 tool guidance | `tool_guidance.md` | The same band |
| C6 skills | `recipes/<name>/RECIPE.md` | The recipe index; `recipe_view` loads one on demand |
| C7 scripts | `scripts/<file>`, `recipes/<name>/scripts/<file>` | Named in the band; the smith runs them with `bash`, in the shell sandbox |
| C8 plugin | `hooks.toml` | Command hooks around each tool call, in the read-only sandbox |
| C5 subagents | none | slag's agent loop has no delegation tool, so the validator drops `sub_agents/` |

A hook line uses the `[hooks]` syntax, such as `post_tool = "name=lint matcher=edit_file cmd='sh $SLAG_HARNESS/scripts/lint.sh'"`. `$SLAG_HARNESS` points at the harness directory. An empty harness, or a missing one, changes nothing: every prompt stays byte-identical to a run without one.

### Task level: `--evolve K`

1. **Reference.** K fix attempts under the current harness.
2. **Propose.** For each of K, an invariant editor (procedure, tool use, verification) and a domain editor (what a practitioner of this kind of repository knows) each edit a copy of the harness.
3. **Probe.** Each candidate runs the same issue on the same model, in its own throwaway copy of the repository.
4. **Compose.** K composers read every run and every candidate and write a successor harness: merge edits, adopt one candidate, or keep the base. Each decision names the run moment that supports it.
5. **Execute and select.** Each successor runs. With K = 1 the composed run delivers when it passes. With K > 1 a blind selector picks among the verified fixes without knowing which harness made them.

`--sequential` chains K propose, probe and compose steps, each building on the last checkpoint, instead of running K in parallel. A candidate whose harness equals one already run reuses that run instead of paying for another.

Every run is one fix attempt in a copy of the working tree, uncommitted work included. slag verifies each run exactly as the fast path verifies a fix: its own test run, the lost-test guard, and the reversal check. Those exit codes are behavioral evidence for the composer, never a reason to skip the comparison. The chosen fix then lands in your repository and passes the same checks there before it commits. When no run verifies, the standard attempts continue under the composed harness.

### Harnessing level: the library

After the fix, a reflector reads the whole record. It writes one row per edited component, with a verdict of `helped`, `hurt`, `no observed effect` or `confounded`, and at most one lesson: a cue for the kind of situation and an operation on a named component. The library gains an entry only when slag's own measurements back a `helped` or `hurt` row: the runs under the harnesses that carried the edit beat the reference runs (or fell behind them) on the first measure that separates them, in this order: whether the fix verified, whether its new test catches the bug, tool errors, turns. The lesson must also name a harness component, and its evidence must cite at least one run of the round by its label (`base-1`, `inv-1`). A verdict the numbers contradict stays the reflector's opinion.

slag sets the entry's confidence from its own counts, and keeps the reflector's figure beside it:

| Confidence | When |
|------------|------|
| high | 3 or more runs that carried the edit agree on whether the fix verified or its test caught the bug, against 2 or more reference runs, and no carrying run disagrees |
| medium | 2 or more carrying runs agree on that outcome measure, and none disagrees |
| low | anything less: one run against one, a difference in turns or tool errors alone, or runs that disagree. The entry says "could be noise" and why |

Two candidates can edit one file in different ways, so a row that names its candidates counts only the runs under those candidates, or under a harness holding the same bytes. Each entry holds the adopted harness, the components of any rejected candidate whose edit measurably helped (an edit that hurt stays on record in the rows, never in a tree), and an EXPERIENCE.md. An INDEX.md lists the entries per domain:

```text
~/.config/slag/library/<domain>/INDEX.md
~/.config/slag/library/<domain>/<entry>/EXPERIENCE.md
~/.config/slag/library/<domain>/<entry>/harness/
~/.config/slag/library/<domain>/<entry>/candidates/<label>/
```

The domain is the repository's stack (`python`, `node`, `rust`, `go`, `shell`, or `general`), read from the test command and the manifests; `--domain` overrides it. A repository can keep its own library at `.slag/library`. Later `--evolve` runs start from the base harness, and their editors and composers read the library as examples. `--harness library` applies the domain's most relevant entries without a probe and with no model call. Entries rank by the words their title, cue and lesson share with the issue, with a bonus for the same repository. The best entry always applies, and up to two more when they share a word with the issue or come from the same repository. Every component of an applied entry comes over: its adopted harness, then the components of rejected candidates that helped. Always-on files from several entries stack, best entry first, up to the 8000-byte cap; a recipe or script name keeps the first entry's copy; hook lines join. From another repository, the lines that state a fact "in this repository" stay behind. The run prints, and the done screen keeps, which entries applied and what each gave:

```text
note    harness from the python library: 20261019-170212-export-drops-the-last-day (memory.md, recipes/ends, systemprompt.md)
```

### Safety and cost

- Improver sessions (editor, composer, selector, reflector) get their evidence as files under `.slag/evolve/<run>/sessions/<name>/context/`. They have file tools rooted there and no shell. slag reads back only `context/harness/` and the reply. An answer that comes before the session opened its evidence (each run's trajectory and checks, each candidate's diff) goes back with the list of files still unread.
- A guard refuses any working directory that holds a grader verdict, a rubric, a score file, a `hidden/` directory, or a bench path. Absolute paths of your repository read as `<repo>`.
- A harness lives outside your repository, so no harness edit can touch your code or tests. Harness scripts run through the smith's sandboxed shell. Harness hooks run in the read-only sandbox without credential variables (`*_API_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`), die at their timeout, and do not run at all where no OS sandbox exists.
- The chosen fix lands in your repository only when every path it changed is a plain path inside the tree, outside `.git`, `.slag` and `logs`, and under no symbolic link. A link at a changed path is replaced, never written through.
- The validator rejects a recipe without a trigger (`description:` of four words or more), a named script the harness does not ship, a script that does not parse, always-on text over 8000 bytes, a hook that is not a plain command, and notes that describe edits, or spell out file writes as text, over a tree that did not change (an unchanged tree needs KEEP_BASE in the notes). A rejected tree gets one retry with the validator's message, then falls back to the tree it started from, and the record says so. A composer whose notes keep edits (`kept -> inv-1 recipes/tests -> ...`) but who wrote none gets those components copied from the named candidates before any retry; the record lists them under `applied`.
- `--evolve 1` costs up to four fix attempts and four improver sessions, plus one for each validation retry. The done screen and `slag status --json` (under `evolve`) show each stage's sessions, time, tokens and spend, as `≈ $0.0123 est.` from the dated price list (or OpenRouter's reported cost). `--evolve-budget USD` (default $2) and `--evolve-tokens N` (default 4,000,000) cap everything the recursion spends. Once either is reached, no further session starts, and both caps stop a session in flight at its next turn. The reference runs are the plain fix, so they always finish; their spend still counts. A capped run says so and delivers the best verified fix found so far.
- slag never invents a price. A call to a model with no list price, no `[pricing]` override and no OpenRouter cost adds nothing to the dollar figure and counts toward the token cap alone. The run names such models up front, and each spend line shows their tokens apart: `≈ $0.0041 est. + 12.3k unpriced tokens`.
- `.slag/evolve/<run>/record.json` keeps the whole record: every run with what slag measured, every candidate with its outcome, the composer's and editors' notes, the selection, and the library outcome.

| Setting | Env | `config.toml` key | Flag |
|---------|-----|-------------------|------|
| Editor model (default: the plan model) | `SLAG_MODEL_EDITOR` | `model_editor` | `--editor-model` |
| Library directory | `SLAG_HARNESS_LIBRARY` | `harness_library` | `--library` |
| Dollar cap (default 2) | `SLAG_EVOLVE_BUDGET` | `evolve_budget` | `--evolve-budget` |
| Token cap (default 4000000) | `SLAG_EVOLVE_TOKENS` | | `--evolve-tokens` |

## Sandbox

The smith's shell commands can read anything and reach the network. They can write only inside the workspace, `/tmp`, your own `$TMPDIR`, and `~/.npm/_logs`. Package caches stay read-only, because each one holds code that a later build outside the sandbox runs: cargo extracts a cached `.crate` without checking it, npm and pip caches serve the metadata that carries their own checksums, and Go's build cache holds compiled objects. Builds that only read the caches keep working: `cargo test --offline` with dependencies already extracted, `npm test`, `python3 -m unittest`. npm, pnpm, Yarn, bun, pip, uv and the Go build cache get a throwaway cache under `$TMPDIR/slag-cache-$USER` instead, so `npm install` and `go test` still work. A `CARGO_TARGET_DIR` outside the workspace becomes `<workspace>/target`. Cargo has no such switch for its registry: a crate that is not already extracted fails with a note that says to run `cargo fetch` outside slag, vendor it, or add the cache to `sandbox_writable`. Nothing on your `PATH` is writable either: not `~/.cargo/bin`, `~/.rustup`, the npx cache, or other users' temp dirs.

Nothing a sandboxed command writes may become code that runs outside the sandbox. slag's own git (commits, snapshots, diffs, worktrees) runs outside it, so the workspace's `.git/hooks`, `.git/config`, `config.worktree`, `commondir` and the `.git` entry itself stay read-only for model commands and for the file tools, and every git command slag runs passes `-c core.hooksPath=/dev/null -c core.fsmonitor=false`. Ordinary git use that writes objects, refs and the index keeps working; `git init` over the workspace and `git config` do not. Landlock cannot carve a read-only file out of a writable tree, so on Landlock slag instead records what the git config would run and refuses its own git once a command changes that. A workspace in a linked git worktree or a repo subdirectory can also write its git dir's objects and refs. The file tools already refuse paths outside the workspace. The sandbox needs no setup:

| Platform | Backend |
|----------|---------|
| macOS | `sandbox-exec` with a generated Seatbelt profile |
| Linux with `bwrap` | bubblewrap: the whole filesystem read-only, the writable paths bound read-write |
| Linux without `bwrap` | Landlock, kernel 5.13 or later |
| Linux with neither | No confinement. slag prints one warning per run that names bubblewrap. |
| Other platforms | No confinement, with one warning per run |

On macOS, `sandbox-exec` fails when slag already runs inside another sandbox. slag then runs unconfined and warns once. On Linux, a `bwrap` that fails to start (no user namespaces) falls through to Landlock. Strict read-only checks require bubblewrap on Linux. Landlock cannot block metadata writes, so slag refuses those checks and warns once.

When the sandbox refuses a write, the tool result tells the model `write outside the workspace was blocked by slag's sandbox: <path>` (or `write to git's hooks or config was blocked by slag's sandbox: <path>`), so it changes approach instead of retrying.

```toml
sandbox = "auto"                               # or "off"
sandbox_writable = ["~/data", "/srv/scratch"]  # extra writable paths
```

`--no-sandbox` or `SLAG_SANDBOX=off` turns it off. Reviewers (the warden) run their shell checks in a stricter sandbox with a read-only workspace and private temporary scratch space. It refuses background commands. `--no-sandbox` and `sandbox = "off"` deliberately do not reach that mode: without a working backend, slag refuses the reviewer's commands rather than run them unconfined. Precedence is flag, then environment, then file. A model writes every `:proof` and `CMD:` line, and `slag fix` can edit the test scripts it then runs, so slag runs proofs, CMD checks, and `slag fix` test runs in the same sandbox. A refused write shows up in the failure as `write blocked by slag's sandbox: <path>`. Test detection's Python probe (`import pytest` with the workspace's `.venv/bin/python` or `python3`) runs in the sandbox too. Hooks, `screenshot_cmd` and `[mcp]` servers stay outside it because you wrote them. They start in the workspace, so a bare `npx` or `python` there can pick up a package the model installed; give them absolute paths if that matters to you.

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
 │                        ├─ warden    --temper: bar, verdict, gap│
 │                        ├─ assay     forged/cracked report      │
 │                        └─ fix       slag fix, --evolve         │
 │                                                                │
 │  engine/  agent loop · tools (read/write/edit/grep/bash)       │
 │           sandbox · hooks · MCP · pricing · recipes · judge    │
 │  harness/ harness loader · validator · library                 │
 │  smith/   NativeSmith factories (work / plan / base)           │
 │  render/ · tui.rs · dashboard.rs   stream · Ratatui · tapes    │
 │  config.rs   key store · model roles · spend caps              │
 └───────────────┬────────────────────────────────────────────────┘
                 │  HTTPS (chat completions or Responses, tool calls)
                 v
          OpenAI-compatible API (OpenRouter by default)
```

The engine runs its own agent loop over OpenAI-compatible chat completions, or the Responses API for OpenAI GPT-5.4 and later. It needs no vendor CLI.

## Commands

| Command | Role |
|---------|------|
| `slag "commission"` | Write `.slag/PRD.md` and run the full pipeline |
| `slag` | Forge the `.slag/PRD.md` already in this directory |
| `slag fix ISSUE` | Fix one issue in this repository and commit it on a new branch |
| `slag key [KEY] [--provider NAME]` | Show provider setup or store its key |
| `slag cost` | Show session spend and an OpenRouter balance when available |
| `slag status [--json]` | Show crucible state. `--json` prints one object: run id, ingots by status, spend, tokens, last event |
| `slag resume` | Resume an existing forge |
| `slag runs` | List past runs from the event logs |
| `slag insights [--refresh]` | Offline analytics over the logs: forged and cracked ingots, heats, spend, tool errors, duel margins |
| `slag ps` | List live forges on this machine |
| `slag rewind [--ingot ID --heat N]` | Restore the workspace from an attempt checkpoint. Bare `slag rewind` takes the most recent attempt |
| `slag replay FILE` | Play a recorded run (`logs/tape-*.jsonl` or an `engine-*.jsonl` log). `--speed`, `--tui`, `--at POINT`, `--hold SECS`, `--size WxH`, `--points` |
| `slag hooks list` | Print the hook events, the exit-code contract, and the configured hooks |
| `slag update` | Self-update to the latest release |

`runs`, `insights`, `ps`, `rewind`, `replay` and `hooks` need no key or network.

## Flags

| Flag | Effect |
|------|--------|
| `--tui` | Full-screen dashboard with crucible, live feed, and steering input |
| `--temper N` | Judge the result against the goal with the warden, up to N rounds (default 0, off). See [Temper](#temper-the-warden---temper-n) |
| `--anvils N` | Max parallel anvil workers (default 3) |
| `--provider NAME` | Select the default provider |
| `--auto` | Reset all four roles to the selected provider default |
| `--model MODEL` | Worker model |
| `--plan-model MODEL` | Planner model for grade 3+ ingots |
| `--judge-model MODEL` | Duel judge model |
| `--duel` | Give every solo ingot at least two casts (same as `SLAG_DUEL=on`) |
| `--trace FILE` | Write a Chrome trace-event file, one lane per anvil, for chrome://tracing or Perfetto |
| `--force` | Accept a one-word commission. Without it, slag refuses a lone bare word as a mistyped subcommand |
| `--worktree` | Branch-per-ingot isolation. Not implemented yet, warns and runs in the shared checkout. |
| `--no-sandbox` | Let shell commands write outside the workspace (see [Sandbox](#sandbox)) |

Flags may come before or after the commission. `slag fix --help` lists the fix flags.

## Environment

| Variable | Effect |
|----------|--------|
| Provider key from the table above | Overrides that provider's stored key |
| `SLAG_PROVIDER` | Default provider; `--provider` takes precedence |
| `SLAG_BASE_URL` | Override the selected provider's API root |
| `SLAG_<PROVIDER>_BASE_URL` | Override one provider's API root in a mixed run |
| `SLAG_MODEL_BASE` / `_PLAN` / `_ALT` / `_JUDGE` / `_EDITOR` | Per-role model ids |
| `SLAG_DUEL` | `auto` (default), `on`, `off` |
| `SLAG_DUEL_ROUNDS` | Override the duel round cap |
| `SLAG_SANDBOX` | `auto` (default) or `off` for the shell sandbox |
| `SLAG_MAX_COST_INGOT` | Dollar ceiling for a single ingot session (file key `max_cost_per_ingot`). Default uncapped. |
| `SLAG_MAX_COST_RUN` | Dollar ceiling for the whole run (file key `max_cost_per_run`). Default uncapped. |
| `SLAG_CREDIT_FLOOR` | Warn at forge start when the OpenRouter balance is under this many dollars (file key `credit_floor`) |
| `SLAG_COST_PER_MTOK` | Flat dollars per million tokens that spend caps charge for a response with no known price (default 5) |
| `SLAG_MODEL_FALLBACK` | OpenRouter fallback model for capacity errors, sent in the same request (file key `fallback_model`) |
| `SLAG_UNATTENDED_RETRY` | `1` keeps retrying 429 and 529 errors past the normal budget, with backoff up to 5 minutes (file key `unattended_retry`) |
| `SLAG_MAX_RETRIES` | Provider request attempts (default 8) |
| `SLAG_API_TIMEOUT_MS` | Provider request timeout in milliseconds (default 300000) |
| `SLAG_REASONING_EFFORT` | `low`, `medium`, `high` for models that take it |
| `SLAG_SCREENSHOT_CMD` | Shell command producing a screenshot for visual assay of web ingots |
| `SLAG_CONFIG_DIR` | Move the config directory off `~/.config/slag` |
| `SLAG_OPENROUTER_BASE` | Legacy OpenRouter-only base URL override |
| `SLAG_CHAR_BUDGET` | Transcript char budget before an agent session compacts (default 48000, less for a small model window). Lower it for small-context models. |
| `SLAG_BASH_OUTPUT_CAP` | Bytes of shell output the smith sees per command (default 30000, ceiling 204800) |
| `SLAG_ALLOW_DESTRUCTIVE` | `1` lets the smith run commands the destructive-command gate refuses, such as `rm -rf` |
| `SLAG_DISABLE_HOOKS` | Turn off every hook for this run (file key `disable_all_hooks`) |
| `SLAG_HARNESS_LIBRARY` | Harness library directory for `slag fix --evolve` and `--harness library` (file key `harness_library`) |
| `SLAG_EVOLVE_BUDGET` / `SLAG_EVOLVE_TOKENS` | Dollar and token caps for `slag fix --evolve` (file key `evolve_budget` for dollars) |
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
| `:bar` | string | The ingot's acceptance bar. With `--temper`, the warden judges it after the proof passes. Empty means the proof alone decides. |
| `:work` | string | Task description for the smith |
| `:duel` | `t` / `nil` | Twin-cast override. `t` forces a duel, `nil` blocks one, absent defers to `SLAG_DUEL`. Sequential ingots never duel either way. |
| `:casts` | 1 / 2 / 3 | Cast-count override. Absent defers to `SLAG_DUEL` and the grade heuristics. |

The parser accepts these thirteen fields and keeps any other `:key value` pair you add, writing it back unchanged.

## Files

| File | Role |
|------|------|
| `.slag/PRD.md` | Requirements input (ore) |
| `.slag/BLUEPRINT.md` | Surveyor analysis output |
| `.slag/PLAN.md` | Ingot crucible (task list) |
| `.slag/AGENTS.md` | Coordinator scratch notes. The coordinator creates a `## Alloy Recipes` stub. Forge prompts read this file. |
| `.slag/BAR.md` | Acceptance bar and required checks. The warden judges against it. |
| `.slag/PROGRESS.md` | Work history ledger |
| `.slag/logs/` | Debug logs (slag heap): `engine-*.jsonl` event logs, `tape-*.jsonl` recordings for `slag replay`, attempt checkpoints for `slag rewind`, the `slag insights` cache |
| `.slag/checks/` | Verification probes and fixtures. Builders write here, reviewers cannot, and product tests must work without it. |
| `.slag/session-costs.json` | Spend per run, for `slag cost`, `slag status --json` and the dashboard |
| `.slag/layout` | Marker that pins the `.slag/` layout |
| `.slag/evolve/<run>/` | The `slag fix --evolve` record (`record.json`) and improver sessions. `.slag/evolve/latest.json` copies the last record. |
| `.slag/library/` | Optional per-repository harness library |
| `recipes/<name>/RECIPE.md` | The real recipes. Each one is a markdown file with `---` frontmatter (name, description, optional requires_tools). slag indexes `./recipes/` and `~/.config/slag/recipes/` and offers the list to the smith. |

Projects that already keep `PLAN.md` or `PRD.md` at the root use the same names at the root, with logs in `logs/`.

## Requirements

- A key for your provider, or a local Ollama or LM Studio server
- `git` (slag commits each forged ingot)

`install.sh` reads the latest release tag from the GitHub API, downloads `slag-<arch>-<os>.tar.gz`, unpacks the `slag` binary into `~/.slag/bin`, and tells you to add that directory to your PATH. The published v2.9.0 release carries one target: `slag-aarch64-apple-darwin.tar.gz` (macOS on Apple Silicon), alongside the same binary uncompressed and a `sha256.sum`. The next release ships four targets: Apple Silicon, Intel Mac, and Linux on x86_64 and aarch64. `install.sh` and `slag update` already handle all four. Until then, other platforms build from source:

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
