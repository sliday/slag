# slag gauntlet: results

Run: 2026-10-15 to 2026-10-19. Base: v2.9.0 (`b6948d3`). Branch: `gauntlet/production` (171 commits, 62 source files changed, +28k lines in `slag-rs/src`).

Method: every piece had a bar a critic could inspect, a builder, and a separate critic with fresh context that judged the real artifact (blind where possible, votes asked in both label orders from round 4 of the late lanes). The full round log is `workbench.md`; harness quirks are `instruments.md`.

## Verdicts

| Piece | Bar | Final verdict |
|---|---|---|
| `slag fix` (issue harness) | `codex exec`, gpt-6.1-sol, held-out 8-task bench | **Won** (r5, model for model): 16/16, median 32 s, p90 40 s, $0.52 vs codex 8/8, 80 s, 155 s; blind fx votes slag 6, codex 1, split 1 |
| Provider-agnostic engine | aider, opencode | **Won** (r4): 14 presets incl. OpenAI frontier models via the Responses API; 5/6 providers forged live; host isolation proven by proxy; spend estimates match published prices |
| TUI flow | Codex CLI, Claude Code, same task | **Won** (r4, blind, fair set): first in 5 of 6 groups incl. both done screens |
| Hero video | Warp, Zed, Charm, Copilot CLI launch videos | **Won** (r1, r2 blind: first of five); every on-screen line from real runs |
| slag.dev page | Zed, Copilot CLI, Warp, Charm | Truth pass; the critic's one-line gap (mobile diagrams) and two second-tier items fixed by the lead |
| Shell sandbox | Codex `workspace-write` / `read-only` | Three escape-hunt rounds; every found escape closed with a regression test; macOS and Linux (bwrap, Landlock) suites green; the fourth critic halted itself on a model safeguard |
| Commission path (`slag "<sentence>"`) | `codex exec`, same model | **Bar won** (0-4-3 in the last two rounds). Cost fell from 2.28M tokens / $2.44 to ~110k / $0.12 per small commission; integrity passes; codex still wins blind taste votes |
| Harness² (`--evolve`, `--harness library`) | the paper's comparison plus a matched-compute control | PENDING (critic r2) |
| Release | `install.sh` on every platform it names | Four-target build script with a publish guard (not published) |

## What changed, by area

- Providers: one setting picks the provider (flag > env > file); `provider::model` per role; OpenRouter-only features gated; no credential crosses providers; deterministic errors stop at the first call with provider, role, model and remedy; shared jittered backoff; dated price table; OpenAI Responses transport for gpt-5.4+ and gpt-6.x.
- `slag fix <issue>`: text, `#123` or URL; reproduces with a new test the repo's own runner executes, fixes minimally, proves it both ways, commits only its own paths on `slag/fix-*`, `--pr` for a draft PR. Guards: no rewriting existing tests, runner-reach probe, literal call first, no stale bytecode, atomic reproduce swap.
- Sandbox: Seatbelt / bwrap / Landlock for every model-controlled command (shell tools, proofs, fix test runs, probes, read-only wardens); git hooks/config neutralized; no package cache writable; `--no-sandbox` escape hatch.
- Commission path: runnable contracts, read-only grounded wardens (a verdict must cite its own successful runs), `.slag/` for coordination files, fresh-clone delivery checks, honest UNVERIFIED.
- TUI: `slag replay` with deterministic frames; one presenter for live and replay; verdict-first done screens; evidence wraps instead of truncating.
- Harness² (arXiv 2610.14224): file-based execution harness, Propose → Probe → Compose for `slag fix --evolve K`, reflector + experience library, `--harness library`, honest budget and confidence.
- Engineering: 21 compiler warnings → 0; clippy `-D warnings` clean; flaky tests removed; Linux portability; `slag replay` no longer hangs in pipes.

## Verification (lead, final wave)

- macOS: `cargo test` 3 runs, 1269 tests each, 0 failed; `cargo clippy --all-targets -- -D warnings` clean; `cargo build --release` OK (prints `slag 2.9.0`).
- Linux (Docker `rust:1-bookworm`, root, bubblewrap): full suite green; Landlock-only sandbox tests green. Non-root (UID 1000) green per the round-10 builder; the lead's own non-root rerun could not start because Docker Desktop failed to restart.
- Website: `npm ci && npm run build` OK; mobile width 390/390; one `<h1>`.

## Open gaps (honest list)

1. Commission taste: on the same model, codex supplies the obvious expectations of a tool kind (case-insensitive words, configurable caps, lenient whitespace); slag's fixes landed per prompt.
2. `slag fix` on js-notify-preview splits by code points; compound emoji can still be cut (the hidden suite lacks that case).
3. Groq's free tier (8k TPM, 200k/day) throttles real runs.
4. Harness² composer often keeps the base harness; improvement evidence rests on small samples.
5. The installed v2.9.0 on PATH turns `slag fix --help` into a paid commission (old parser); fixed in this branch, so cut the release.

## Next commands (owner decides)

```sh
# Release (bump version first, e.g. 3.0.0, then):
cd slag-rs/dist-local && ./publish-release.sh --build && ./publish-release.sh --check
git tag v3.0.0 && git push origin v3.0.0
./publish-release.sh "slag 3.0.0" "<notes>"
# Website:
cd website && npm run build && npx wrangler pages deploy dist --project-name=slag-dev
```
