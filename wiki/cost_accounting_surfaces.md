---
name: cost_accounting_surfaces
desc: Where slag-rs prices calls, accumulates spend and reports it — and the one choke point any new spend accounting must hook, since the judge and summarizer bypass the agent loop.
tags: []
sources: []
created: 2026-08-26T11:25:00Z
updated: 2026-08-26T11:25:00Z
---

# cost_accounting_surfaces

Orientation for any node touching cost, tokens or budget in `slag-rs`.

## The layers

| Layer | Lives in | Answers |
|-------|----------|---------|
| Price table | `engine/pricing.rs` `PricingTable` | what a model charges per token |
| Per-call attribution | `engine/provider.rs` `attribute()` | which model and call site spent it |
| Run ledger | `engine/pricing.rs` `CostLedger`, held in the `engine::stats` static cell | what each (model, role) pair spent this run |
| Caps | `ForgeAgent::track_spend` and the spend-tracking provider wrapper | when to refuse the next call |
| Readouts | `pipeline/assay.rs` (`spend` heading), `dashboard.rs`, `cli.rs` `slag cost` | what the operator sees |

Prices and context windows both come from a single `GET /models` fetch,
parsed into `ModelsIndex`. There is a test that asserts `/models` is hit
exactly once (`invoke_fetches_the_model_window_to_size_the_token_budget`);
adding a second fetch is the way to trip it.

## Hook spend at the provider, not the agent loop

`provider.rs` `attribute()` runs on the single success return out of the
retry loop in `chat()`. Every call reaches it: smith turns, duel casts,
the judge, the summarizer.

`ForgeAgent` does **not** see every call. `judge.rs` and `compact.rs`
(via `summarize`) hold a `Provider` directly and never enter the agent
loop. Accounting placed in the loop silently drops their spend — the
numbers still render, they are just wrong and low.

This has bitten the repo twice:

- `summarize` once hit the raw provider and bypassed the ingot
  accumulator, so the ingot and run caps under-counted. The regression
  test `summarizer_spend_lands_in_the_ingot_accumulator` in `agent.rs`
  guards it.
- The cost ledger first folded at the two `EngineEvent::Tokens` emit
  sites in `ForgeAgent`, so judge and compaction rows never appeared —
  in the ledger whose stated purpose is making judge spend visible.
  `every_call_site_reaches_the_ledger_not_just_the_smith` in
  `provider.rs` guards it.

New spend accounting goes in `attribute()`. If you must put it in the
agent loop, write a test that drives a non-smith role through it.

## `:free` is not a variant like the others

OpenRouter model ids carry suffixes: `vendor/model:free`,
`:nitro`, `:floor`, `:online`. Lookups fall back from a suffixed id to
the bare one when the exact id is missing.

That fallback is correct for **context windows** — every variant of a
model shares its window — and wrong for **prices** across `:free`, which
bills nothing by definition. An unlisted free variant (stale disk cache,
a proxy that trims the model list) inheriting the paid base rate prints a
bill nobody owes. `PricingTable::lookup` refuses the fallback for
`:free` and keeps it for routing variants; `ModelsIndex::window` keeps it
for all of them. Any new id-normalizing helper needs the same split.

## Numbers that are guesses say so

`Usage` carries `estimated: bool` alongside `cost: Option<f64>`. A cost
the provider reported prints `$0.0123`; one derived from the local table
prints `~$0.0123 (est)`. `Usage::add` propagates the taint — one
estimated leg makes the sum an estimate. Render through
`pricing::format_cost()` rather than formatting `cost` directly, or the
`(est)` marker gets dropped at a new call site.

A free model estimates to `None`, not `Some(0.0)`: a `$0.0000 (est)`
line reads as a bug rather than as a free call.

## Extending `Usage` is safe; extending `EngineEvent::Tokens` is not

Attribution rides on `Usage.model` / `Usage.role`, both
`#[serde(default)]`, rather than on new `EngineEvent::Tokens` fields.
That kept every `Tokens { usage }` match arm compiling untouched and old
JSONL deserializing. Prefer widening `Usage` over widening the event.
See [[slag_rs_event_and_render_surfaces]] for the append-only contract on
the enum itself.

One thing widening does break: a `Usage { .. }` struct literal, which
`#[serde(default)]` does nothing for. Those literals live almost
entirely in test code, so a plain `cargo check` reports success while
`cargo test` fails to compile. Build test fixtures as
`Usage { prompt_tokens: 1, ..Usage::default() }` so the next field
lands free, and gate any merge that crosses this struct on `cargo check
--all-targets`.
