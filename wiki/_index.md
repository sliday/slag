---
name: slag
desc: Shared knowledge for slag orchestrator nodes — ops lessons and cross-node reference.
tags: []
sources: []
created: 2026-08-25T23:11:45Z
updated: 2026-08-26T13:14:03Z
---

# slag

[[bash_guard_and_analytics_surfaces|bash_guard_and_analytics_surfaces]]: Where slag's bash guard rails, background bash, and offline analytics live — orientation map for nodes touching engine/tools.rs or the logs heap.

[[child_node_context_overflow|child_node_context_overflow]]: Failure pattern — child dies every step on `agent error (exit 1)`; two causes (context overflow, exhausted model credits) told apart by accrued cost.

[[cost_accounting_surfaces|cost_accounting_surfaces]]: Where slag-rs prices calls, accumulates spend and reports it — and the one choke point any new spend accounting must hook, since the judge and summarizer bypass the agent loop.

[[hook_engine_surfaces|hook_engine_surfaces]]: The lifecycle hook engine in slag-rs — its exit-code protocol, the four hook kinds, and the seams it binds to in agent.rs, forge.rs, and cli.rs.

[[inspiration_100_backlog_audit|inspiration_100_backlog_audit]]: The inspiration-100 backlog is closed; how its checkboxes drifted from the tree while it was open, which modules the CLAUDE.md layout table still omits, how a _shipped: note goes wrong in both directions, and the shared conventions for state under ~/.slag/.

[[slag_rs_event_and_render_surfaces|slag_rs_event_and_render_surfaces]]: Where the EngineEvent enum actually lives, the append-only contract on it, and the render/ module's terminal-free shape.

***

- [[child_node_context_overflow]] — child dies every step on "Prompt is too long": diagnosis and remedy.
- [[inspiration_100_backlog_audit]] — backlog checkboxes lag the tree; grep before implementing.
- [[cost_accounting_surfaces]] — hook spend at the provider; the judge and summarizer skip the agent loop.
