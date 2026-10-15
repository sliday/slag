Complexity: hard — OS-level process confinement on two platforms, wired into the agent's shell tool without breaking ordinary builds and test runs.

# Lane: confine the smith's shell writes to the workspace

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-sandbox (git worktree, branch gauntlet/sandbox). Work only here.

## Context

slag is a Rust coding-agent orchestrator in `slag-rs/`. Read `CLAUDE.md` first, then `slag-rs/src/engine/tools.rs` (the `bash`/shell tool and the path-confined file tools around line 564) and `slag-rs/src/engine/policy.rs` (config-driven allow/deny rules for commands). File tools already refuse paths outside the workspace. The shell tool runs model-written commands in the workspace root with no OS-level confinement: an independent critic watched a model run `find /Users/stas -name proof.rs` and `grep -rn PLAN.md /Users/stas/Playground/...` from inside a forge. Reads like that waste time; a write like that could destroy user data. slag now also runs `slag fix <issue>` inside users' real repositories.

The bar is Codex CLI's `workspace-write` sandbox (Seatbelt on macOS, Landlock/seccomp on Linux): model commands can write only inside the workspace and temp dirs.

## Goal

Commands the smith runs cannot write outside the workspace (plus temp and the user's tool caches), on macOS and Linux, with no setup, while normal build and test commands keep working.

## Deliverables

1. macOS: run shell-tool commands under `sandbox-exec` with a generated profile: allow by default, deny `file-write*` everywhere except the workspace root, the system temp dirs (`/private/tmp`, `/private/var/folders`, `$TMPDIR`), `/dev/null` and ttys, and the common tool caches under `$HOME` (`.cargo`, `.rustup`, `.npm`, `.cache`, `Library/Caches`, `.gradle`, `.m2`, `go`, `.bun`, `.pnpm-store`, `.local/share/pnpm`, `.yarn`). Network stays allowed (tests may need localhost).
2. Linux: use `bwrap` when installed (bind the whole fs read-only, bind the workspace, temp and caches read-write); otherwise Landlock if you can do it without new heavy dependencies; otherwise run unconfined and print ONE clear warning per run naming what to install. Document which applies.
3. Config: `sandbox = "auto" | "off"` in config.toml, `SLAG_SANDBOX` env, `--no-sandbox` flag (precedence flag > env > file, same pattern as other settings). `auto` is the default. Extra writable paths via `sandbox_writable = ["..."]`.
4. When a command fails because the sandbox denied a write, the tool result the model sees must say so plainly ("write outside the workspace was blocked by slag's sandbox: <path>") so it changes approach instead of retrying blindly. Detect it from the exit status/stderr text that sandbox-exec and bwrap produce.
5. Tests: unit tests for profile generation and precedence; an integration test (macOS-gated, skipped elsewhere) proving `touch $HOME/slag-sandbox-probe` fails under the sandbox while `touch ./ok`, `cargo --version`, `python3 -c 'import tempfile; tempfile.mkstemp()'` and writing under `$TMPDIR` succeed. Clean up any probe file.
6. Check real builds still pass under the sandbox: run the existing `bench/` once with the slag-fix adapter on cerebras for the Rust, Node and Python tasks (`SLAG_BIN=<your debug build> SLAG_PROVIDER=cerebras bench/run.sh --agent bench/agents/slag-fix.sh --tasks rust-hawkconf,js-leaderboard,py-passcheck`), keys are in the environment, never print them, spend ≤ $0.20. Report pass/fail and times.
7. Docs: a short "Sandbox" section in `README.md` and the config table in `CLAUDE.md`.

## Paths you own (edit nothing else)

`slag-rs/src/engine/tools.rs` (shell execution path only), new `slag-rs/src/engine/sandbox.rs`, `slag-rs/src/engine/mod.rs` (module registration), `slag-rs/src/config.rs` (the sandbox setting only), `slag-rs/src/cli.rs` (the `--no-sandbox` flag only), tests, `README.md` and `CLAUDE.md` sections. Do NOT touch `engine/provider.rs`, `pipeline/`, `tui.rs`, `dashboard.rs`, `render/`, `website/`, `bench/`.

## Hard rules

- Minimal diffs; match code style and comment density. Prose: no em dashes, active voice, short sentences.
- Commit on `gauntlet/sandbox` as `sandbox: one sentence`. Do not push. No version bump.

When done, list the files you changed, the exact commands that verify the work, the bench table, and what Linux does when bwrap is missing.
