---
name: hook_engine_surfaces
desc: The lifecycle hook engine in slag-rs — its exit-code protocol, the four hook kinds, and the seams it binds to in agent.rs, forge.rs, and cli.rs.
tags: [hooks, slag-rs, surfaces]
created: 2026-08-26T09:17:53Z
updated: 2026-08-26T09:17:53Z
---

# hook_engine_surfaces

`slag-rs/src/engine/hooks.rs` holds the whole lifecycle hook engine
(items 69-77 of `notes/inspiration-100.md`). Anything that fires a hook,
adds an event, or touches the call sites should read this first.

## The exit-code protocol

One vocabulary for every hook kind, so a caller never has to know which
kind ran:

| Code | Meaning |
|------|---------|
| `0` | stdout becomes model-visible context (or JSON: `updated_input` / `additional_context`) |
| `2` | block; stderr goes to the smith |
| anything else | logged and ignored |
| `CODE_FAILED = -1` | timeout or un-spawnable. Negative, so it cannot collide with a real exit status |

**A broken hook never blocks.** A gate that errors, an agent that dies, a
webhook returning 500, a timeout — all land on `CODE_FAILED`. That rule is
load-bearing: it is why a misconfigured hook cannot wedge a forge, and any
new kind must honour it.

## The four kinds

`HookKind` is parsed from one shell-quoted `[hooks]` config line. Exactly
one kind per line — two is a confused config and the line is dropped
rather than guessed at.

| Kind | Config key | What runs |
|------|-----------|-----------|
| `Command(cmd)` | `cmd=` | `sh -c`, payload JSON on stdin |
| `Prompt{prompt, model}` | `prompt=` | one LLM call through `judge::rule`, on the judge model |
| `Agent{prompt, model}` | `agent=` | a grade-1 smith with tools; refuses via a leading `BLOCK:` line |
| `Http{url, headers, allowed_env}` | `url=` | POST the payload JSON on provider.rs's reqwest stack |

`judge::rule(provider, model, instruction, payload) -> Ruling{block, reason}`
lives at the end of `engine/judge.rs`. It is `assay`'s cheap sibling: same
no-tools framing, same outermost-brace lenient parse, same
one-retry-then-error discipline, but `Effort::Low` — a gate fires on every
matching event, so one costing more than the work it guards does not get
used.

HTTP reuses HTTP's own vocabulary instead of inventing a second one: 2xx
accepts, `403` refuses, everything else is the webhook's problem.

## Header interpolation is allowlist-only

`interpolate()` expands `$NAME` and `${NAME}` in header values **only** for
names listed in `allowedEnvVars`. Every other name resolves to the empty
string, so a config file cannot exfiltrate an unlisted variable by naming
it. Built by a single left-to-right scan, not regex replacement: an
expanded value that itself contains `$OTHER` stays text and is never
expanded on a second lap. The recipe span expander
(`engine/recipes.rs::expand_shell_spans`) follows the same discipline for
the same reason.

## The seams

| File | Seam |
|------|------|
| `engine/agent.rs` | `dispatch_hooked()` — a free function called from `spawn_call`'s spawned task. Pre → dispatch → post/error. **The turn loop is untouched.** |
| `pipeline/forge.rs` | `fire_ingot_hook()`, a helper at the end of the file, called at all five `EngineEvent::IngotDone` emit sites. `hooks` is already a local variable name there, so spell the module `crate::engine::hooks::`. |
| `cli.rs` / `main.rs` | `Hooks { action }` + `show_hooks()`, both appended at the END of their blocks. |
| `dashboard.rs`, `events.rs` | `HookStarted` / `HookFinished` arms, appended at the end of their matches. |

`EngineEvent` lives in **`engine/mod.rs`**, not `engine/events.rs`. A new
variant needs arms in both `events.rs` and `dashboard.rs` or the build
breaks in two places.

`reqwest` comes from `provider.rs` — never add a Cargo.toml line for it.
That append has been a three-way conflict more than once.

## Config

`config.rs` exposes `hook_entries()`, `disable_all_hooks()`, and
`truthy()`. The snapshot is frozen once in a `OnceLock` at session start,
so a smith editing `slag.toml` mid-run cannot register a hook into its own
running session. `install_snapshot()` seeds one for tests.

## Test convention

Snapshot-driven: `snap(&[(event, spec)], disabled)` builds a `HookSnapshot`
from literal config lines, then `fire_with` drives it. Tests never touch
the process-wide `OnceLock`, so they do not contend for it. Two gotchas:

- `once` tests need globally unique hook names — the spent set is
  process-wide.
- Env-var tests need uniquely-named variables (`SLAG_TEST_HOOK_TOKEN_*`),
  because `cargo test` runs parallel and env is process-wide.

Mocks: `smith/mock.rs` `MockSmith::fixed()`/`failing()` is public.
`judge.rs`'s `MockJudge` is private to its own tests module, so `hooks.rs`
carries a local `MockGate` provider.

## Sibling surface: inline shell in recipes

Item 78 is not part of this engine. `engine/recipes.rs` runs inline
`` !`cmd` `` spans and ` ```! ` blocks through the same bash executor
(`tools.rs`), then splices stdout by building the output string manually —
never by regex-replacing with command output, which would let untrusted text
rewrite the surrounding recipe. Read `substitute_args` in that file for the
scanner it copies.
