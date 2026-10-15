Complexity: hard — cross-module change to the engine's only provider seam, config precedence, key handling and OpenRouter-only features; must stay backward compatible with ~822 existing tests.

# Lane: provider-agnostic engine

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-provider (a git worktree on branch gauntlet/provider). Work only here.

## Context

slag is a Rust task orchestrator in `slag-rs/`. Read `CLAUDE.md` at the repo root first: it documents the architecture, the config table and the source layout. Then read `slag-rs/src/config.rs` and `slag-rs/src/engine/provider.rs`.

Today slag is OpenRouter-only:
- `engine/provider.rs` has `struct OpenRouter` that speaks the chat-completions API.
- `config.rs` loads `openrouter_api_key`; every model role defaults to `AUTO_MODEL` (`openrouter/auto`); `SLAG_OPENROUTER_BASE` overrides the base URL.
- OpenRouter-only behavior: the `reasoning` body object, `reasoning_details` replay, `HTTP-Referer`/`X-Title` headers, the credits endpoint (`slag cost`), the key-check endpoint (`slag key`), the 402 "out of credit" text, `openrouter/auto`, and pricing lookups.
- `main.rs` refuses to forge without an OpenRouter key.

## Goal

slag works with any OpenAI-compatible chat-completions provider, chosen with one setting, and makes no OpenRouter call when another provider is in use. The owner wants a fast, LLM-provider-agnostic harness. Speed matters: fast inference providers (Cerebras, Groq) must work well.

## Deliverables

1. Provider presets, each with a base URL and its standard key env var: `openrouter` (default; behavior unchanged), `openai` (OPENAI_API_KEY), `anthropic` (OpenAI-compatible endpoint, ANTHROPIC_API_KEY), `gemini` (OpenAI-compatible endpoint, GEMINI_API_KEY), `groq` (GROQ_API_KEY), `cerebras` (CEREBRAS_API_KEY), `xai` (XAI_API_KEY), `mistral` (MISTRAL_API_KEY), `deepseek` (DEEPSEEK_API_KEY), `together` (TOGETHER_API_KEY), `fireworks` (FIREWORKS_API_KEY), `ollama` (localhost:11434/v1, no key), `lmstudio` (localhost:1234/v1, no key), and `custom` (any base URL, optional key). Use the base URLs you know from each provider's docs; list any you are not sure of in your final message.
2. Selection: `provider = "<name>"` in config.toml, `SLAG_PROVIDER` env, `--provider` flag. Precedence flag > env > file, the same pattern the model roles use. A `base_url` override must work for every preset (keep `SLAG_OPENROUTER_BASE` working as an alias).
3. Mixed providers in one run: a per-role model setting must be able to name its provider (for example plan on openai, work on cerebras). Pick a syntax that cannot be confused with existing OpenRouter ids such as `openai/gpt-5`, which must keep working unchanged when the provider is openrouter. Document the syntax.
4. Defaults: when the provider is not openrouter and no model is set, use a sensible default model per preset (document each). `openrouter/auto` stays the OpenRouter default.
5. Keys: `slag key` shows the active provider and whether its key is present, and can store a key for a named provider. The pre-forge key gate in `main.rs` accepts any provider that has a key, and keyless local providers. Never print a key value.
6. Gate the OpenRouter-only features listed above so other providers never receive them. A non-OpenRouter provider must not get body fields that make it return 400 (the `reasoning` object is the known one); send the provider's own equivalent where one exists (OpenAI-style `reasoning_effort`) or omit it. `slag cost` must degrade to session spend only, with a one-line note, for providers without a balance endpoint.
7. Robustness: honor 429 `Retry-After` with bounded backoff; show provider error bodies in readable form; handle tool-calling quirks generically (providers that reject `tool_choice` or `parallel_tool_calls`, or reject an assistant message with empty `content` plus `tool_calls`).
8. Tests: wiremock tests for each behavior (body shape per provider, header gating, precedence, key resolution, defaults, mixed-provider roles, 429 retry). `cd slag-rs && cargo test` must pass. Set `CARGO_PROFILE_DEV_DEBUG=line-tables-only` to keep the target dir small.
9. Live smoke, if network is available: keys for openrouter, openai, cerebras, groq, gemini, xai and mistral are in the environment. Never print them. For each, run one minimal chat-completions call with one tool through slag's own provider code (a small `#[ignore]` test or an example binary is fine) and record pass/fail plus latency in your final message. Keep total spend under $1.
10. Docs: update the configuration section of `CLAUDE.md` and the provider/setup section of `README.md` to match. Keep the edits tight.

## Paths you own (edit nothing else)

`slag-rs/src/config.rs`, `slag-rs/src/engine/provider.rs`, `slag-rs/src/engine/pricing.rs`, `slag-rs/src/engine/mod.rs`, `slag-rs/src/smith/**`, `slag-rs/src/main.rs` (key gate and `run_key` only), `slag-rs/src/cli.rs` (the `--provider` flag and the `Key` subcommand only), `slag-rs/src/error.rs` if needed, new test files under `slag-rs/tests/`, `README.md` and `CLAUDE.md` (provider/config sections only).

Do NOT touch `tui.rs`, `dashboard.rs`, `render/`, `pipeline/`, `website/`, or `Cargo.toml` version. Other builders own those.

## Hard rules

- Minimal diffs. Match the surrounding code style, naming and comment density. No reformatting of untouched code.
- Prose you write (docs, comments, commit messages): no em dashes; active voice; short sentences.
- Commit on branch `gauntlet/provider` in this worktree, in the repo's message style (`area: one sentence`, e.g. `provider: speak to any OpenAI-compatible endpoint`). Several commits are fine. Do not push. Do not bump the version.
- Do not delete or rewrite existing tests to make them pass; change a test only when the behavior it pins deliberately changed, and say so.

When done, list the files you changed, the exact commands that verify the work, the live smoke table, and anything you are unsure of.
