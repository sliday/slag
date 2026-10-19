slag 3.0.0 works with any provider, fixes issues end to end, and runs every model-controlled command in a sandbox.

## New

- **Any provider.** 14 presets, including OpenRouter, OpenAI, Anthropic, Groq, Cerebras, Mistral, Ollama and LM Studio. One setting picks the provider (flag, then env, then `config.toml`), and `provider::model` pins a single role. OpenAI gpt-5.4 and later, GPT-6.x included, go through the Responses API. Run lines show a dated spend estimate.
- **`slag fix <issue>`.** Give it text, `#123` or an issue URL. It reproduces the bug with a new test that the repo's own runner executes, makes a minimal fix, proves the test fails without the fix and passes with it, then commits only its own files on a `slag/fix-*` branch. `--pr` opens a draft PR. Guards: no rewriting of existing tests, a check that the new test really runs, and no stale bytecode.
- **Shell sandbox.** Shell tools, proofs, fix test runs and read-only wardens run under Seatbelt (macOS), bubblewrap or Landlock (Linux). Git hooks and config are neutralized, and package caches are not writable. `--no-sandbox` turns it off.
- **Commissions.** Runnable contracts, read-only wardens whose verdicts must cite their own successful checks (`--temper N`), and a fresh-clone delivery check. Coordination files live under `.slag/`. A small commission now costs about 110k tokens; it used to cost 2.28M.
- **Terminal flow.** Verdict-first done screens, `slag replay` for recorded runs, and `slag runs`, `insights`, `ps`, `rewind` and `hooks`.
- **Experimental: `slag fix --evolve K` and `--harness library`.** These implement Harness² (arXiv 2610.14224). On slag's bench, `--evolve` has not beaten plain `--tries 4` at matched compute, and it costs 30 to 500 times a plain fix. The README has the numbers.

## Changed

- Shell commands run in the sandbox by default.
- New commissions keep PRD, BLUEPRINT and PLAN under `.slag/`. Projects that already have them at the root keep them there.
- Flags after the commission now parse as flags: `slag "build x" --temper 1`.
- `slag fix ...` is a subcommand. v2.9.0 treated it as a commission text.

## Fixed

- SIGTERM (`timeout`, a CI cancel, `docker stop`) now runs the cleanups and stops the shell commands the agent started, then exits 143.
- slag no longer re-sends an empty assistant turn, which Mistral and Anthropic reject.
- `--evolve` keeps a fix that has already verified when a later stage fails.

## Platforms

macOS on Apple Silicon and Intel, Linux on x86_64 and aarch64 (glibc 2.17 or newer). Install with `curl -sSf https://slag.dev/install.sh | sh`, or run `slag update`.
