Complexity: standard — a list of small, well-specified production cleanups across the crate; verify each by running it.

# Lane: production smoothing

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-smooth (git worktree, branch gauntlet/smooth). Work only here.

slag is a Rust coding-agent orchestrator in `slag-rs/` (read `CLAUDE.md` first). Many builders changed it over the last days; these loose ends remain. Do each one; skip none silently. For each, say what you did and how you verified it.

1. Compiler warnings: `cargo build` reports 36 warnings for the bin (baseline v2.9.0 had 21). Bring it to 0 for `cargo build` and `cargo build --release`, and make `cargo clippy --all-targets -- -D warnings` pass. Remove dead code rather than silencing it, unless the code is used under a cfg or by tests (then scope the allow narrowly and say why).
2. `slag-rs/src/shutdown.rs` tests drain every hook registered in the process, so they can fire other modules' hooks during a parallel `cargo test`. Isolate them (a local registry for tests, or run the drain against a private instance) without changing production behavior.
3. On Linux without `rg`, `glob_matches_patterns_with_directory_components` (engine/tools.rs tests) fails. Make the glob tool work without `rg` (fall back to a built-in walk) or, if the tool legitimately requires rg, make the tool report a clear error and make the test skip with a printed reason when rg is missing. Prefer the fallback.
4. Find tests that depend on wall-clock timing and can fail under heavy load (one forge test timed out once at load average 30). Make them deterministic (paused tokio time, generous bounds, or event-driven waits). Run the full suite while loading the machine (e.g. `yes > /dev/null` x 16 for the duration, then kill them) to show it holds.
5. `slag fix` prints the commission banner `survey → plan → forge → assay` (main.rs / tui banner). Show a banner that fits the fix flow (for example `reproduce → fix → prove → commit`), or no subtitle.
6. README.md says "Duels only fire when cast B runs a different model", which contradicts the CLI's own duel line and the site ("1 to 3 casts by grade and work, direction prompts"). Read `pipeline/duel.rs` and make README and CLAUDE.md state what the code does.
7. Provider pre-flight: an unknown `--model` id is only discovered after the survey and founder have spent money on the plan model. Before the first paid call, check every configured model id against the provider's `/models` list when the preset has one (cache it per process; never call another provider's host; skip silently for presets without a list), and stop with the same remedy line the 404 path prints.
8. Grammar nit: a message reads "a openai host" (search the crate for `"a {`-style article bugs and fix the pattern).

Rules: minimal diffs, match code style and comment density; prose with no em dashes, active voice, short sentences. `cd slag-rs && CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test` passes by cargo's own exit code (do not pipe it through grep to judge success). Keys are in the environment for item 7 (use `--provider mistral` or `openai`; Cerebras has no credit; never print keys; spend ≤ $0.10). Commit each item as its own commit on `gauntlet/smooth` (`area: one sentence`). Do not push. No version bump.

When done, list each item with its commit, what changed, and the command that proves it.
