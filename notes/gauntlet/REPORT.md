# slag gauntlet: results

Run: 2026-10-15 to 2026-10-19. Base: v2.9.0 (`b6948d3`). Branch: `gauntlet/production` (175 commits, 62 source files changed, +28k lines in `slag-rs/src`).

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
| Harness² (`--evolve`, `--harness library`) | the paper's comparison plus a matched-compute control | **Bar won** (r2): `--evolve 1` 6/16 vs plain `--tries 4` 7/16 at matched compute; library transfer 6/16 vs 5/16, inside noise; 30-500x the cost of a plain fix. Shipped as experimental with the measured result in the README. The critic's gap (a failed probe discarded a verified fix) is closed with a regression test |
| Release | `install.sh` on every platform it names | Four-target build script with a publish guard (not published) |

## What changed, by area

- Providers: one setting picks the provider (flag > env > file); `provider::model` per role; OpenRouter-only features gated; no credential crosses providers; deterministic errors stop at the first call with provider, role, model and remedy; shared jittered backoff; dated price table; OpenAI Responses transport for gpt-5.4+ and gpt-6.x.
- `slag fix <issue>`: text, `#123` or URL; reproduces with a new test the repo's own runner executes, fixes minimally, proves it both ways, commits only its own paths on `slag/fix-*`, `--pr` for a draft PR. Guards: no rewriting existing tests, runner-reach probe, literal call first, no stale bytecode, atomic reproduce swap.
- Sandbox: Seatbelt / bwrap / Landlock for every model-controlled command (shell tools, proofs, fix test runs, probes, read-only wardens); git hooks/config neutralized; no package cache writable; `--no-sandbox` escape hatch.
- Commission path: runnable contracts, read-only grounded wardens (a verdict must cite its own successful runs), `.slag/` for coordination files, fresh-clone delivery checks, honest UNVERIFIED.
- TUI: `slag replay` with deterministic frames; one presenter for live and replay; verdict-first done screens; evidence wraps instead of truncating.
- Harness² (arXiv 2610.14224): file-based execution harness, Propose → Probe → Compose for `slag fix --evolve K`, reflector + experience library, `--harness library`, honest budget and confidence. A stage error after a run verified now stops the recursion and delivers that run.
- Robustness from the last critic: slag never re-sends an empty assistant turn (Mistral and Anthropic reject it); SIGTERM (`timeout`, CI cancel, `docker stop`) runs the cleanups, and the cleanups kill the smith's running shell commands.
- Engineering: 21 compiler warnings → 0; clippy `-D warnings` clean; flaky tests removed; Linux portability; `slag replay` no longer hangs in pipes.

## Verification (lead, final wave)

- macOS: `cargo test` 1272 tests, 0 failed (earlier wave: 3 runs of 1269, 0 failed); `cargo clippy --all-targets -- -D warnings` clean; `cargo build --release` OK (prints `slag 2.9.0`). New SIGTERM test: 10 of 10 in a stress loop; red without its fix.
- Linux (Docker `rust:1-bookworm`, root, bubblewrap): full suite green before the last two fixes; Landlock-only sandbox tests green. Non-root (UID 1000) green per the round-10 builder. After the last two fixes, Docker Desktop stopped answering, so the lead ran only a cross-build: `cargo zigbuild --target x86_64-unknown-linux-gnu --all-targets` compiles the binary and every test crate with no warnings. The Linux test run for those two commits is still owed.
- Website: `npm ci && npm run build` OK; mobile width 390/390; one `<h1>`.

## Open gaps (honest list)

1. Commission taste: on the same model, codex supplies the obvious expectations of a tool kind (case-insensitive words, configurable caps, lenient whitespace); slag's fixes landed per prompt.
2. `slag fix` on js-notify-preview splits by code points; compound emoji can still be cut (the hidden suite lacks that case).
3. Groq's free tier (8k TPM, 200k/day) throttles real runs.
4. Harness² shows no gain over plain retries at matched compute. Its library admits lessons backed only by tool-error deltas from unfixed tasks, and `--harness library` applies the best entry to every issue regardless of fit.
5. The installed v2.9.0 on PATH turns `slag fix --help` into a paid commission (old parser); fixed in this branch, so cut the release.

## Release 3.0.0 (published 2026-10-20)

- Owner pushed tag `v3.0.0` (→ `6c86174`), published the release, and deployed slag.dev. GitHub marks v3.0.0 latest, with 8 assets plus `sha256.sum`.
- Lead checks after publish: `slag update` took the owner's v2.9.0 to v3.0.0, and the installed binary's SHA-256 equals the built `slag-aarch64-apple-darwin`; `curl -fsSL https://slag.dev/install.sh | sh` installs `slag 3.0.0` into a clean macOS home and into a clean `debian:stable-slim` amd64 container; the live site serves the new install line and the Harness² row.

- `slag-rs/Cargo.toml` is 3.0.0; commit `63f47d4` holds the bump, the README/CLAUDE.md/site release lines, and all four targets in `slag-rs/dist-local` (`publish-release.sh --check`: 8 assets pass).
- Runtime: `slag 3.0.0` from each binary on its own platform (Linux arm64 and amd64 in `debian:stable-slim` under Apple `container`, Intel Mac under Rosetta, Apple Silicon native).
- Linux suite, non-root (UID 1000) with bubblewrap under Apple `container`, rust 1.99.1: 1274 passed, 0 failed, every test binary. One earlier run failed `pipeline::duel::tests::transient_provider_blips_do_not_fail_a_cast` once; it passed alone and in the full rerun. Treat it as a rare flake to chase.
- rust 1.99 warns that `AtomicU64::fetch_update` (`engine/provider.rs:1300`) is deprecated; the local 1.98 toolchain lacks `try_update`, so the change waits for a toolchain bump.
- Notes: `notes/gauntlet/release-3.0.0.md`.

## Next commands (owner decides)

```sh
# Release (bump version first, e.g. 3.0.0, then):
cd slag-rs/dist-local && ./publish-release.sh --build && ./publish-release.sh --check
git tag v3.0.0 && git push origin v3.0.0
./publish-release.sh "slag 3.0.0" "<notes>"
# Website:
cd website && npm run build && npx wrangler pages deploy dist --project-name=slag-dev
```
