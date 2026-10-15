Round 3. A fresh critic re-ran everything on the integrated build. Good news first: all six providers forge end to end (mistral 7 s, cerebras 19 s, openai 20 s, openrouter 61 s, groq 233 s throttled, gemini 248 s), host isolation is clean, mixed roles work, every run terminates. The bar still wins on ONE gap. Fix it, re-verify live, commit.

Your worktree was just synced with `gauntlet/production` (it now contains the `slag fix` pipeline and the forge-loop fix). This round you may also edit `slag-rs/src/pipeline/forge.rs`, `slag-rs/src/pipeline/resmelt.rs`, `slag-rs/src/pipeline/fix.rs` and `slag-rs/src/pipeline/fix/**` for the short-circuit only; keep those diffs minimal. Keys are in your environment; never print them; spend ≤ $1.

BIGGEST GAP: a deterministic provider error inside the forge is treated like a flaky cast, and the message does not help.
- `--model nope-model-404` on cerebras: survey and founder pass on the plan model, then the smith gets 404 five times (heat 1/5 to 5/5), a re-smelt pass spends 31k plan tokens rewriting the ingot (its rewrite even says "previous heats failed on a provider/model 404"), then CRACKED after 26 s. Same with a wrong key for the work provider in a mixed run: 401 five times, re-smelt, crack. Evidence: `pipeline/forge.rs` around line 895 (`crucible.increment_heat`) has no short-circuit on auth or invalid-request errors.
- Messages: `provider error: invalid key (401): Wrong API Key` (no provider name, no remedy); `provider error (transient): gave up after 3 attempts: request failed ... http://localhost:1234/...` (no "lmstudio", no "start the server"); xAI's credits-exhausted 403 comes back labelled "invalid key (403)" (`provider.rs` around line 723 maps `401 | 403 => Auth`). Remedies exist only for OpenRouter (`provider.rs` around lines 591-597).

Required behavior:
1. Classify provider errors into: auth (bad or missing key), billing (credits or spend limit, detected from status 402 or from the body text on 403/429), model not found / invalid request (deterministic), rate limit (retry with backoff, already done), transient (network, 5xx). Classification must use the body text, not only the status code.
2. Deterministic classes (auth, billing, model not found) stop the run at the first occurrence, in every pipeline (commission forge, survey, founder, warden, `slag fix`): no heats burned, no re-smelt, no duel. Exit non-zero with one clear block.
3. Every error message names the provider preset and the role/model involved and gives the remedy for that preset: which env var or `slag key --provider <name>` to set, the credits/billing URL, `ollama serve` / "start LM Studio's local server" for local presets, "check the model id; list models with ..." for unknown models.
4. Tests (wiremock) for each class, including xAI-style billing text on 403, and a forge-level test proving a 401 burns zero heats and zero re-smelt calls.
5. Live: rerun `--model nope-model-404` on cerebras and a bad key on groq; report wall time to failure and the exact message printed.

Commit on `gauntlet/provider` with `provider: ...` / `forge: ...` messages. Report files changed, the messages printed, and verification commands.
