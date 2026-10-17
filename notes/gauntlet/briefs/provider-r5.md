Round 5. Your provider lane is merged into `gauntlet/production` and your worktree was synced again (it now has the TUI redesign, the shell sandbox and `slag fix`). Keys are in your environment; never print them; spend ≤ $0.20.

The next gap, raised by five separate reviewers: every non-OpenRouter run prints `spend -`. A harness that runs paid calls must say what they cost, or say honestly that it cannot.

Rules that still bind: no request may reach openrouter.ai (or any host of another preset) while another provider is active. So do NOT fetch OpenRouter's catalog to price a Cerebras run.

Required:
1. Pricing for non-OpenRouter presets from a built-in table of list prices for each preset's default models and a handful of common models per preset (USD per million input/output tokens; cached-input price where the provider publishes one). Put the date the prices were taken in the table and in a code comment. Mark every such figure as an estimate in the UI ("≈ $0.0031 est.").
2. User override: a `[pricing]` table in config.toml keyed by `provider::model` (input, output per million). Override wins.
3. When no price is known, print tokens instead of a dash: `spend unknown · 62.5k in / 6.0k out tokens` (wording up to you, but never a bare `-`). The cached OpenRouter `pricing.json` may still be used for the openrouter preset only, as today.
4. Apply it everywhere spend shows: stream footer, dashboard header, assay, `slag fix` summary, `slag cost`, `slag status --json`. Do not restyle those screens (the TUI builder owns their layout); change only the value they print. Coordinate by keeping the formatting function in `engine/pricing.rs` and calling it.
5. Tests for table lookup, override precedence, the unknown-price fallback, and that pricing never triggers a network call off OpenRouter (wiremock or a trap base URL).
6. Live: one `slag fix` on a tiny scratch repo with `--provider cerebras` and one with `--provider openai`; report the spend lines printed and compare against the provider's own published price for the tokens used.

`cargo test` green (read cargo's own exit status). Commit on `gauntlet/provider` (`pricing: ...`). Report files changed, the spend lines, and the prices you used with their source and date.
