slag 3.1.0 fixes the five ways 3.0.0 broke on real codebases. On the same model, builds now cost less than a plain coding agent.

Measured with hidden tests on OpenAI `gpt-6.1-sol`, 3 runs per task, against 3.0.0 and against codex on the same model:

| | 3.0.0 | 3.1.0 | codex |
|---|---|---|---|
| Real-repo bug fixes (5, 15k-93k lines) | 12/15 pass | 15/15 pass | 15/15 |
| Semver library build | $1.54, 15 min | $0.10, 2 min | $0.20, 2.5 min |
| Spreadsheet engine build | $3.90, 31 min | $0.17, 3.5 min | $0.29, 4.8 min |
| Rust CLI build | $2.07, 21 min | $0.14-0.40, 3-8 min | $0.23, 3 min |

## `slag fix`

- **Fixes the bug class, not only the example.** The agent tries variants of the reported input, and the review checks that the tests cover more than the literal report (pycparser case: 0/3 to 3/3).
- **Hang bugs.** The check without the fix gets a timeout sized from the earlier test runs instead of the full 300 s. A timeout proves the bug only when the issue reports a hang; otherwise slag reruns once at the full ceiling. `SLAG_TEST_TIMEOUT` sets that ceiling.
- **Existing tests that encode the bug.** An edit to an existing test lands only with a review answer whose quotes slag checks against the issue and the diff: one row per changed check, no check weakened to drop its expected value. The proof keeps new tests added inside an edited test file.
- **No proof, no FIXED.** A new test that passes without the fix gets one revision request, then NOT FIXED.
- The review gets 45 s instead of 10 s.

## Builds

- **Detailed builds take the direct path.** Commissions up to 8,000 characters in a folder holding only docs run as one strong session (80 turns) with one criterion per stated interface, instead of survey, plan and 3-12 serial ingots. Without `--temper`, FORGED rests on the agent's own suite and probes; `--temper N` adds the independent reviewer and the fresh-clone run.
- **Context.** The agent keeps its full history while under budget, so the provider's prompt cache keeps hitting. The budget comes from the model window, at most 200k tokens, where the provider caches. Elsewhere, including Ollama, LM Studio and Anthropic direct, it stays at 12k. Claude through OpenRouter gets `cache_control` breakpoints (78% of prompt tokens served from cache in a measured build).

## Duels and cost

- `SLAG_DUEL=off` is a kill switch: it beats `:casts` and `:duel` pins. The planner no longer writes those pins.
- Auto duels only when the alt model differs from the work model; an unset alt follows the work model; `--auto --model X` keeps alt on X. `slag key` says why a duel is idle.
- Reviewer, judge and hook-gate calls now appear in spend and token events.

## Other fixes

- Grammar lines in a blueprint no longer count as surveyor questions, and real questions such as "(cursor-based)?" still do.
- A finish call at the turn cap counts as the answer.

Install with `curl -sSf https://slag.dev/install.sh | sh`, or run `slag update`. Builds ship for macOS (Apple Silicon and Intel) and Linux (x86_64 and aarch64, glibc 2.17 or newer).
