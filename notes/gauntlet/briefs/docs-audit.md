# Docs truth audit (Sonnet auditor, 2026-10-19; lead-verified where marked)

Binary prints `slag 2.9.0`.

## README.md
False or stale:
- L366 and the diagram near L361: "chat completions only". OpenAI gpt-5.4+ use `/responses` (provider.rs:867-873, :635); README L88 already says so.
- L383-393 flags table misses `--temper N`, `--force`, `--trace FILE`, `--duel` (cli.rs:23-90).
- L368-379 commands table misses replay, runs, insights, ps, rewind, hooks; L377 `slag status` also takes `--json`.
- L435 "eleven fields": the parser has 13 (sexp/parser.rs:4-8); `:bar` and `:casts` missing from the L421-433 table although L206 uses `:casts`.
- L170 "4-phase pipeline": `--temper N` adds the warden stage (pipeline/mod.rs:528-650); L175 diagram omits it.
- L455 "one target": true for the published v2.9.0; the next release builds four (publish-release.sh --build); install.sh and update.rs:190-203 handle all four.
- L395-417 env table misses SLAG_MODEL_FALLBACK, SLAG_UNATTENDED_RETRY, SLAG_CREDIT_FLOOR, SLAG_MAX_RETRIES, SLAG_API_TIMEOUT_MS, SLAG_BASH_OUTPUT_CAP, SLAG_COST_PER_MTOK, SLAG_ALLOW_DESTRUCTIVE, SLAG_DISABLE_HOOKS.
- L437-448 files table misses `.slag/layout`, `.slag/session-costs.json`, `.slag/evolve/`, `.slag/library`, `logs/tape-*.jsonl`.
Verified true: presets, base URLs, key vars, defaults (config.rs:261-315); `provider::model`; Responses cutoff; GPT-6.1 Sol / GPT-5.5 price rows; `SLAG_<PROVIDER>_BASE_URL`; `--auto` resets four roles (main.rs:67); all `slag fix` flags the README names.
Missing: `replay` (--speed --tui --at --hold --size --points), `runs`, `insights [--refresh]`, `ps`, `rewind [--ingot --heat]`, `hooks list`; lifecycle `[hooks]` and the exit-code contract (0 injects context, 2 blocks) and `SLAG_DISABLE_HOOKS`; `--temper`, `--trace`, `--force`, `--duel`; `slag fix --tui`, `--model`, `--plan-model`.

## CLAUDE.md
- L9 chat-completions-only vs L129 Responses.
- L19 "4-Phase" omits temper.
- L32 "eleven fields" (13); L34-46 lacks `bar`, `casts`; L183 uses `:casts`.
- L50-62 source layout omits render/, replay.rs, insights.rs, migrations.rs, harness/; L57 pipeline list omits warden, direct, hygiene.
- L75-77 file table omits .slag/layout, session-costs.json, evolve/, library.
- L179 env list misses the nine vars above.
- L193 binary surface omits cost, replay, runs, insights, ps, rewind, hooks, and --temper/--force/--trace/--duel, and fix flags --pr/--tries/--test/--issue-file/--no-commit/--temper/--tui/--model/--plan-model.
- L222-225 website structure omits public/media/ and website/video/.
Correct: provider table, install.sh copy is byte-identical, sandbox/harness/.slag sections.

## website/index.html, src/*.js
- L415 "Those eleven are every field the parser reads": false (13).
- L172-183, L236-257: diagram and phase cards use root PRD.md/BLUEPRINT.md/PLAN.md; current commissions use `.slag/` (config.rs:51-55).
- L229 "Four Phases" omits temper; L68 (video aria) mentions `--temper 1`: the page contradicts itself.
- L122 "published build is macOS Apple Silicon": true today; stale at the next tag.
- L423-457 CLI reference lacks replay, runs, insights, ps, rewind, hooks and --temper, --force, --trace, --duel, --no-sandbox.
- L462-481 env table lacks SLAG_SANDBOX, SLAG_MODEL_EDITOR, SLAG_<PROVIDER>_BASE_URL, SLAG_HARNESS_LIBRARY, SLAG_EVOLVE_BUDGET, SLAG_EVOLVE_TOKENS; L270-306 role table lacks the editor role.
- Missing on the page: the shell sandbox and its escape hatch (L121 undersells), --evolve/--harness, fix flags --tries/--test/--temper/--tui, spend estimates and `[pricing]`, gpt-5.4+ via Responses incl. GPT-6.1 Sol, replay, `.slag/`, `[mcp]`, hooks.
Correct: 14 presets, `provider::model`, `slag/fix-*` branches, --pr/--no-commit, duel 1-3 casts; content.js and main.js have no false claims.

## bench/README.md
No false claims; minor gap: the slag-fix adapter defaults SLAG_CONFIG_DIR to an empty temp dir and the README does not say so.

## Lead-verified extras
- Hooks are read from `config.toml` (`[hooks]`, config.rs:1133); `engine/hooks.rs:5` and the `slag hooks list` hint at `engine/hooks.rs:967` wrongly say `slag.toml`.
- `slag-rs/.github/workflows/{ci,release}.yml` sit under `slag-rs/`, not the repo root, so GitHub never runs them (pre-existing since v1.2.0). The real release path is `slag-rs/dist-local/publish-release.sh`.
