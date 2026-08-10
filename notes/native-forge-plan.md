# Native Forge Plan — slag as self-contained coding machine

> STATUS 2026-08-10: v1 core SHIPPED via ultracode workflow — i1–i7 + i11 done (engine/ 3.5k lines, 118 tests green, key gate wired, NativeSmith default when OPENROUTER_API_KEY set). Remaining: i8–i10 (Ratatui dashboard, steering), i12 (interview genesis), i13–i15 (duels), i16 (recipes).

Goal: slag runs own agent loop. No `claude`, `codex`, `opencode`, `kimi` subprocess. Provider: OpenRouter only. One API key. Reasoning patterns borrowed from opencode (sst/opencode, MIT) and hermes (NousResearch, open source).

## 1. Why this works now

- `Smith` trait (`slag-rs/src/smith/mod.rs:11`) is the only AI boundary: `invoke(prompt) -> Result<String>`.
- `ClaudeSmith` spawns `claude -p` subprocess. Replace with `NativeSmith` that runs internal agent loop. Zero changes to surveyor/founder/forge/assay pipeline.
- Retry machinery already exists at ingot level (`:heat`, `:max`, resmelt). Proof-based verification (`:proof` shell commands) is model-agnostic. Slag's edge stays: bad model output gets caught by proofs, not trusted.

Difference vs old design: `Smith::invoke` returns final text only. Native smith does full tool-use loop internally, returns final summary text. Interface unchanged.

## 2. Architecture — new module `slag-rs/src/engine/`

```
engine/
  provider.rs    — OpenRouter HTTP client
  tools.rs       — native tool implementations + JSON schemas
  agent.rs       — agentic loop (the smith brain)
  compact.rs     — context window management
smith/
  native.rs      — NativeSmith: implements Smith, wraps engine
```

### provider.rs — OpenRouter client

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`. OpenAI-compatible.
- Headers: `Authorization: Bearer $OPENROUTER_API_KEY`, `HTTP-Referer: https://slag.dev`, `X-Title: slag`.
- Request: `model`, `messages`, `tools`, `tool_choice: auto`, `reasoning: { effort }` (OpenRouter unified reasoning param — works across o-series, R1, Claude thinking, Gemini thinking).
- Response: assistant message with optional `tool_calls[]`, optional `reasoning` field, `usage`.
- v1 non-streaming (simpler, forge runs headless anyway). v2 adds SSE streaming for TUI progress.
- Retry: 429/5xx → backoff, max 3 attempts, then hard fail (circuit breaker rule). Ingot-level heat handles the rest.
- Crates: `reqwest` (json), `serde`/`serde_json`. Tokio already present. Keep deps tiny.

### tools.rs — the hands

Toolset mirrors opencode/hermes minimal core:

| Tool | Args | Notes |
|------|------|-------|
| `read_file` | path, offset?, limit? | line-numbered output, cap ~2000 lines |
| `write_file` | path, content | create or overwrite |
| `edit_file` | path, old_string, new_string | exact-match replace; error if 0 or >1 matches (opencode-proven pattern) |
| `bash` | command, timeout? | cwd = anvil worktree, merge stdout+stderr, truncate tail ~30KB |
| `grep` | pattern, path? | ripgrep-style; v1 can route through bash |
| `finish` | summary | explicit loop terminator |

- Schemas as static JSON (serde_json::json! blocks). Dispatch by name. Results appended as `role: tool` messages.
- Sandbox: all paths resolved inside anvil worktree; reject `..` escapes. Bash inherits worktree cwd (worktree isolation already exists in `anvil/worktree.rs`).

### agent.rs — the loop

```
messages = [system(slag forge identity + workspace rules + proof contract),
            user(ingot work + repo context)]
loop:
  resp = provider.chat(model, messages, tools)
  if resp.tool_calls:
      for call: result = tools.dispatch(call); messages.push(tool_result)
      continue
  else: return resp.text        # or finish tool summary
guards: max_turns (40), token budget, wall-clock timeout
```

- System prompt: port structure from opencode's coding prompt + hermes harness style. Emphasize: satisfy `:proof` command, minimal diffs, no questions (self_iterate becomes obsolete — bake "decide, never ask" into system prompt; keep `has_questions` as belt-and-suspenders).
- Failure → `SlagError::SmithFailed` → existing heat/resmelt retries the ingot fresh. Free self-healing.

### compact.rs — context management

- Track `usage.total_tokens` per call. Near model limit → drop or summarize oldest tool results first (keep system + original task + last N turns intact). Both opencode and hermes do variants of this. v1: truncate old tool outputs to one-line stubs. v2: model-generated summary turn.

## 3. Plan mode replacement (grade >= 3)

`--permission-mode plan` maps to two-phase natively:

1. **Survey pass**: reasoning model, read-only tools (`read_file`, `grep`, `bash` limited to read-only), output = plan text.
2. **Execute pass**: base model, full tools, plan injected into context.

Same shape serves SURVEYOR (blueprint) and FOUNDER (ingot generation) — those are single-shot prompts; they can even run tool-less pure completion.

## 4. Config

Extend `SmithConfig` (`config.rs`):

```
OPENROUTER_API_KEY        required for native mode
SLAG_MODEL_BASE           default: qwen/qwen3-coder  (cheap, strong at tools)
SLAG_MODEL_PLAN           default: deepseek/deepseek-r1 or openai/gpt-5 (reasoning)
SLAG_MODEL_AUTO=1         optional: openrouter/auto routes per request
SLAG_REASONING_EFFORT     low|medium|high, default medium; grade maps to effort (grade 1-2 → low, 3 → medium, 4-5 → high)
SLAG_SMITH                legacy CLI escape hatch, kept one release, then removed
```

Selection: `OPENROUTER_API_KEY` set → `NativeSmith`. Else fall back to `ClaudeSmith` with deprecation warning. Grade + skill pick model and effort instead of CLI flags.

Note on subscriptions: OpenRouter = one key, pay-per-token, all frontier + open models. True OAuth subscriptions (Claude Pro, Copilot — the opencode trick) need per-provider auth flows. Provider stays behind a small trait so a future `provider/anthropic_oauth.rs` can slot in. Out of scope for v1 per instruction.

## 5. What we borrow (all open source, check licenses on port)

- **pi** (pi.dev, badlogic/pi-mono, MIT, TypeScript): primary reference. It is the smallest clean harness of the three. Packages: `pi-ai` (multi-provider LLM API), `pi-agent-core` (loop + tool calling + state), `pi-coding-agent` (CLI). It has print mode (`-p`, JSON event stream), RPC mode (stdin/stdout JSONL), and an SDK. Custom providers cover OpenRouter (OpenAI-compatible baseURL + key).
- **opencode** (MIT): edit-tool exact-replace semantics, tool schema shapes, compaction trigger points.
- **hermes** (Nous): lean harness loop, reasoning-trace passthrough (store `reasoning` field in logs/), decide-don't-ask prompt discipline.
- Not the code — the patterns. Rust implementation stays ours, ~1.5–2k lines total.

## 5a. Hermes deep-dive — mechanisms to port (local source review, ~/.hermes/hermes-agent)

All of these live inside `engine/`. The `Smith` trait boundary does not move. Pipeline untouched.

### Into v1 (engine core)

| # | Mechanism | Hermes source | slag home |
|---|-----------|--------------|-----------|
| 1 | **Edit fuzzy ladder.** Exact-replace first, then 3–4 fallback strategies (line-trimmed, whitespace-normalized, indentation-flexible). Refuse similarity strategies under `replace_all`. Escape-drift guard: reject when a fuzzy match plus stray `\'`/`\"` from JSON serialization would corrupt the write. No-op detection (`is_already_applied`). On failure return whitespace-visualized near-miss hint. | `tools/fuzzy_match.py:149,256,67,1012` | `tools.rs::edit_file` |
| 2 | **Per-model edit-format steering.** GPT/codex-family models get patch-mode guidance; Claude/Qwen/Kimi/DeepSeek/etc get str-replace guidance. slag switches models per grade — prompt must switch with them. | `coding_context.py:172` | system prompt builder |
| 3 | **Coding operating brief.** Three sections: gather context first; edit through tools not chat; verify then stop (max ~3 attempts on same file, then report). | `coding_context.py:217` | system prompt, stable tier |
| 4 | **Workspace snapshot + project facts.** Git branch, dirty counts, last 3 commits, detected verify commands (from package.json scripts, Makefile, pytest.ini; cap 8). Injected at smith start. Complements `:proof` — the smith sees how to self-check before the proof gate runs. | `coding_context.py:869,780` | agent.rs turn prologue |
| 5 | **write_file returns `verified: true`** (on-disk hash) + syntax check surfacing only newly-introduced errors. Stops the model re-reading its own writes. | `file_tools.py:1757` | `tools.rs::write_file` |
| 6 | **Reader/writer path reservations.** Plan each tool batch into parallel/sequential segments preserving model order: reader↔reader overlap runs parallel, any writer overlap serializes. | `tool_dispatch_helpers.py:116` | agent.rs dispatcher |
| 7 | **Local-vs-API error classification.** A deterministic local bug breaks the loop immediately; only API errors consume retry budget. Backfill unanswered `tool_call_id`s with synthetic error results so the API never sees mismatched IDs. | `conversation_loop.py:7405` | agent.rs error path |
| 8 | **Three-tier prompt banding for prefix cache.** stable (identity, rules) / context (workspace) / volatile (recipes index, timestamp). Timestamp is date-only, so the prompt stays byte-stable all day and OpenRouter prompt caching holds. | `system_prompt.py:152,540` | prompt builder |

### Into v2

| # | Mechanism | Hermes source | slag home |
|---|-----------|--------------|-----------|
| 9 | **Recipes = the hermes skills system, metallurgy-named.** `recipes/<name>/RECIPE.md` with YAML frontmatter (name, description, requires_tools, fallback_for_tools, platforms). Index rendered as a names+descriptions block at the *front of the volatile tier*. Model-driven loading via a `recipe_view` tool — no keyword matching. Disk snapshot keyed on an mtime+size manifest; survives restarts. Demote irrelevant recipes to names-only, never hide (agent-created recipes are project memory). Repeat views of unchanged files return a stub. This is the praised feature — it becomes slag's extension system. | `prompt_builder.py:1602,1388,1555,1811`, `skills_tool.py:1802` | `engine/recipes.rs` + `AGENTS.md` alloy |
| 10 | **Three steering channels** for the TUI (refines i10): `steer` (non-interrupting — text appended into the last tool-result message), `redirect` (cancel in-flight model call, keep completed tool results, retry with correction), `interrupt` (stop, fenced against in-flight compaction commit). | `run_agent.py:3248,3284,3047` | agent.rs + TUI input |
| 11 | **Two-stage live compaction** (refines i6): cheap no-LLM pre-pass first (prune old tool results, strip media), then LLM summary with a Resolved/Pending template; snap cut boundaries off tool-call/tool-result pairs; anti-thrash cooldown after ineffective compressions. | `context_compressor.py:6162,2878,5254` | compact.rs |
| 12 | **NormalizedResponse shape.** `{content, tool_calls, finish_reason, reasoning, usage, provider_data}` — provider quirks quarantined in `provider_data`. OpenRouter reasoning: request `extra_body.reasoning {effort}`, response `reasoning_details`. | `transports/types.py:89`, `chat_completions.py:470,804` | provider.rs types |
| 13 | **Verification stop-guard.** If a turn edited code and tries to finish with no fresh test/lint evidence, emit one bounded nudge (max 3). Suppressed for doc-only turns. Cheap belt under the `:proof` gate. | `verification_stop.py`, `verify_hooks.py` | agent.rs finalizer |

Skipped deliberately: progressive tool disclosure (slag has 6 tools, hermes built it for 3,300), multi-provider profile registry (OpenRouter-only v1), trajectory_compressor (offline training-data tooling, not runtime).

## 5b. pi.dev routes — three ways to employ it

| Route | Shape | Cost | Trade-off |
|-------|-------|------|-----------|
| A: pi as smith backend | `PiSmith` spawns `pi` in RPC mode (JSONL over stdio). OpenRouter via pi custom provider. | ~1 day | Fast. But slag then depends on a Node CLI. This breaks the letter of the "no other CLI tools" rule. |
| B: pi SDK sidecar | Small Node sidecar built on `pi-ai` + `pi-agent-core`. slag-rs drives it over JSONL. | ~3 days | Own tools and prompts. Battle-tested loop. Still needs Node at runtime. |
| C: native Rust engine | Sections 2–4 above. Use pi-mono as the design reference instead of opencode. | ~1-2 weeks | Zero runtime dependencies. Full purity. Most work. |

Recommended: staged. Start with Route A behind the `Smith` trait. That validates OpenRouter models, prompts, and proofs this week. Then build Route C with pi as the reference. The `Smith` trait makes the swap free. Route A code becomes a permanent escape hatch, like `SLAG_SMITH` today.

## 6. Milestones (ready to become ingots in PLAN.md)

| id | work | proof |
|----|------|-------|
| i1 | provider.rs: chat completion + tool_calls parsing + retry | `cargo test -p slag engine::provider` vs mock HTTP server |
| i2 | tools.rs: 6 tools + dispatcher + path sandbox | `cargo test engine::tools` |
| i3 | agent.rs loop + NativeSmith impl Smith | mock provider drives read→edit→finish; test asserts file changed |
| i4 | config wiring, model/effort per grade, ClaudeSmith fallback | env-matrix unit test |
| i5 | plan-mode two-phase | integration test: grade-4 ingot produces plan turn then edits |
| i6 | compact.rs truncation | synthetic long session stays under token cap |
| i7 | live smoke: forge trivial ingot via real OpenRouter (qwen3-coder) | `:proof test -f out.txt` passes end-to-end |
| i8 | TUI: turn/tool/token telemetry in progress.rs; reasoning traces to logs/ | manual + snapshot test |

Dogfood move: write this as `PRD.md`, let current claude-backed slag forge i1–i6, flip env var, forge i7–i8 with itself. The machine builds its own independence.

## 7. TUI

Current state: `slag-rs/src/tui.rs` prints a line stream with `crossterm` + `indicatif`. Palette: cold ore → hot metal → pure steel. Good for CI and logs. Not interactive.

Choice by route (from awesome-tuis survey):

| Library | Language | Fit |
|---------|----------|-----|
| **Ratatui** | Rust | Route C. The standard Rust TUI. Immediate-mode. Uses crossterm backend — already a slag dep. Active ecosystem. **Pick this.** |
| tachyonfx | Rust | Ratatui add-on. Shader-like effects. Gives the forge fire/glow aesthetic. |
| tui-textarea | Rust | Ratatui add-on. Multi-line input for steering messages. |
| iocraft | Rust | Declarative React-like alternative. Younger, smaller ecosystem. Skip. |
| Cursive | Rust | Retained-mode, widget-heavy. Wrong shape for a live dashboard. Skip. |
| OpenTUI / Ink / pi-tui | TypeScript | Only relevant for a Route B Node sidecar. The sidecar stays headless; slag-rs owns the display. Skip. |

Two display modes, like pi and opencode:

1. **Stream mode** (default, today's code): line output, spinners, temper bar. Keeps CI and log files clean.
2. **Dashboard mode** (`slag forge --tui`): full-screen Ratatui app.
   - Left pane: crucible — ingot list with status, heat color, grade color.
   - Right pane: live agent feed — turns, tool calls, tool results (truncated), reasoning traces.
   - Bottom: temper bar, anvil slots (3), token/cost sparkline from `usage`.
   - Input line (tui-textarea): steering messages to the running smith — the pi trick. Queue the message; inject it into the agent loop on the next turn.
   - Event loop: tokio `select!` over crossterm `EventStream` + engine event channel (`mpsc`). Engine emits typed events (`TurnStart`, `ToolCall`, `ToolResult`, `Tokens`, `IngotForged`). Same events serialize to JSONL in logs/ — one event stream feeds TUI, logs, and a future `--json` print mode.
   - Port the existing palette constants into a Ratatui `Style` theme. Keep the metallurgy.

New milestone: replaces old i8.

| id | work | proof |
|----|------|-------|
| i8 | engine event channel + JSONL log sink | `cargo test engine::events`; forge run writes events.jsonl |
| i9 | Ratatui dashboard: panes, theme, temper bar | snapshot test via `insta` + manual run |
| i10 | steering input wired to agent loop | integration test: queued message alters next turn |

## 8. Onboarding — key first, then intent

First run (`slag` with no config):

1. **Key gate.** Prompt for `OPENROUTER_API_KEY`. Validate it with one cheap `/models` call. Store it in `~/.config/slag/config.toml` (chmod 600). Env var overrides file. No key → no forge.
2. **Intent interview.** Ask: "What do you want to build?" The plan model asks at most 3 clarifying questions (tui-textarea form). After that: decide-don't-ask.
3. **Auto-genesis.** Surveyor writes `PRD.md` from the interview. Founder writes `BLUEPRINT.md` + `PLAN.md`. Duel flags set by grade (section 9). User reviews PLAN.md, then `slag forge`.

## 9. Twin-cast forging — A/B dev process (design-studio loop)

Concept from the user's studio practice; closest existing skill is `design-shotgun` (gstack). No dedicated A/B-dev skill exists yet — slag becomes its embodiment.

### Loop per ingot

```
round r:
  cast A: smith in worktree-a (model/direction A)
  cast B: smith in worktree-b (model/direction B)
  gate:   both must pass :proof  (correctness is not judged, it is tested)
  assay:  judge model compares diff-a vs diff-b (+ screenshots for web skill)
          → winner, score margin, critique (incl. what loser did better)
  stop if: margin large, or score plateau, or r == max rounds
  else:    next round; both casts receive winning diff + critique as context
merge winner into main worktree; log duel to PROGRESS.md
```

### Rules that make it affordable and honest

1. **Grade-gated.** Grade 1–2: single cast, cheap model, no duel. Grade 3–4: duel, max 3 rounds. Grade 5 or `:polish t`: studio mode, up to 10 rounds. Override per ingot with `:duel t/nil`.
2. **Convergence beats fixed count.** Judge scores both casts 0–100 each round. Stop early when margin ≥ 20 or when winner score gain < 5 vs last round. 10 rounds is the cap, not the target.
3. **Forced diversity.** Cast A and cast B differ by model family or by direction prompt (e.g. "minimal, fewest lines" vs "robust, defensive"). Two samples of one model converge — that is not A/B.
4. **Judge independence.** Judge = different model family than both smiths, via OpenRouter. Judge never writes code.
5. **Proofs gate, judge ranks.** A cast that fails `:proof` re-heats as today; it never reaches the judge. The judge only compares working solutions on quality: clarity, design, UX, size.
6. **Visual assay** only for `web/ui` skill ingots: headless-browser screenshot per worktree; multimodal judge model compares.
7. **Loser salvage.** Critique lists loser's superior ideas; next round injects them.
8. **Cost ledger.** Duel ≈ 2×rounds smith runs + rounds judge calls. Estimate per ingot before start; write actual spend to PROGRESS.md.

### Role mapping — no external CLIs needed

| User's role | CLI world | slag native (OpenRouter, one key) |
|-------------|-----------|-----------------------------------|
| cheap fast worker | opencode | `SLAG_MODEL_CHEAP` — e.g. `x-ai/grok-code-fast-1`, `openai/gpt-5-mini`, `qwen/qwen3-coder` |
| smart judge / planner | codex | `SLAG_MODEL_JUDGE` — e.g. `openai/gpt-5`, `openai/o4` (reasoning effort high) |
| main smith A/B | claude/other | `SLAG_MODEL_BASE` + `SLAG_MODEL_ALT` (different family for cast B) |

Same brains as codex/opencode, reached through the one OpenRouter key. Optional escape hatch: `CliSmith` config can still point a cast at a local CLI (`codex exec`, `opencode run`) for users who have those subscriptions — pluggable behind the `Smith` trait, off by default.

### Infrastructure already in place

- Worktree isolation: `anvil/worktree.rs`. Duel needs 2 worktrees per ingot (one duel eats 2 of MAX_ANVILS=3 slots — raise to 4).
- Parallel anvils, proof gating, heat/retry, ASSAY phase: all exist. Judge slots into ASSAY naming — the assayer.

### Added milestones

| id | work | proof |
|----|------|-------|
| i11 | onboarding wizard: key gate + validate + config.toml | integration test with mock server |
| i12 | intent interview → PRD/BLUEPRINT/PLAN genesis | golden-file test |
| i13 | duel engine: twin worktrees, rounds loop, convergence stop | `cargo test engine::duel` with mock smiths |
| i14 | assayer: judge prompt, scoring schema, critique threading | mock-judge test: margin/plateau stops honored |
| i15 | visual assay for web ingots (screenshot both, multimodal judge) | e2e on sample web ingot |
| i16 | recipes system: RECIPE.md index, snapshot cache, `recipe_view` tool | `cargo test engine::recipes`; recipe loads mid-session |

Hermes-derived scope notes: i2 absorbs the fuzzy ladder + verified writes (#1, #5). i3 absorbs the operating brief, workspace snapshot, path reservations, error classification, prompt banding (#2–#4, #6–#8). i6 becomes two-stage compaction (#11). i10 becomes three steering channels (#10). Revised estimate: ~2.5–3k lines.

## 10. Risks

- Tool-calling quality varies per model → default base model must be tool-strong (qwen3-coder, kimi-k2, sonnet via OpenRouter all fine); proofs catch the rest.
- OpenRouter rate limits per upstream → retry ≤3 + heat covers it.
- Context overflow on big repos → i6 mandatory before real use.
- Cost: no subscription flat rate; add `usage` accumulation to PROGRESS.md ledger so spend visible per ingot.
- Duel cost: worst case ~6–20× per ingot. Grade gate + convergence stop keep the average near 2–4× on the ~30% of ingots that duel.
- Judge bias: LLM judges favor longer answers and their own family's style. Mitigate: rubric prompt, position swap (judge sees A/B then B/A, must agree), different family than smiths.
- Merge conflicts: casts run from the same base commit; winner merges cleanly by construction. Sequential (`:solo nil`) ingots must not duel in parallel with overlapping files.
