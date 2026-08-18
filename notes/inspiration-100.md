# 100 slag improvements mined from the Claude Code source (2026-08-17)

Source: 8-beat sweep of /Users/stas/Playground/Claude Code src (140 raw ideas, curated to 100).
Status legend: [ ] todo · [~] in flight · [x] done

Progress: wave 1 (items 1-16) + item 18 in v2.2.0. Wave 2 (~12 items) + adaptive casts + site in v2.3.0. Wave 3 (15 items: compaction v3, provider resilience, founder briefing, observability) in v2.4.0. ~45/100 shipped.


## Wave 1

- [x] **1. Adopt Retry-After-aware exponential backoff with jitter** (S)
  engine/provider.rs: replace BACKOFF_MS=[250,1000] fixed array with base-500ms doubling capped at 32s plus 0-25% jitter; parse the Retry-After response header on 429 and let the server directive override the computed delay.
  _evidence: Claude Code src/services/api/withRetry.ts:519-548_
- [x] **2. Broaden retry classification: 408/409/5xx, connect errors, x-should-retry** (S)
  engine/provider.rs chat_impl: extend the '429 || 5xx' check to 408/409/429/5xx plus reqwest connect/timeout errors; honor an x-should-retry header if OpenRouter sends it; expose retryable() on a structured error in error.rs.
  _evidence: Claude Code src/services/api/withRetry.ts:610-621,696-787_
- [x] **3. Stop charging heats for transient provider errors** (S)
  pipeline/forge.rs:386-404: classify SlagError::Provider into transient (429/5xx/timeout) vs permanent; transient failures get a separate counter and do NOT increment :heat or run proof/judge. Prevents network weather cracking an ingot in 20 seconds.
  _evidence: Claude Code src/query.ts:1258-1265; slag-rs/src/pipeline/forge.rs:386-404_
- [x] **4. Treat empty or truncated 200 responses as retryable** (S)
  engine/provider.rs normalize(): a 200 with empty choices, null message, or missing finish_reason must classify as a retryable Provider error inside the attempt loop instead of passing through as an empty assistant turn.
  _evidence: Claude Code src/services/api/claude.ts:2337-2364_
- [x] **5. Synthesize error tool_results for orphaned tool calls on abort** (S)
  engine/agent.rs: when cancel or a provider/tool error aborts mid-batch after tool_calls arrived, append is_error tool_results for every unanswered call before returning, so next-heat transcripts never carry dangling tool_calls (OpenAI-format backends 400 on replay).
  _evidence: Claude Code src/query.ts:123-149,984,1025_
- [x] **6. Enforce hard per-ingot and per-run spend caps** (S)
  config.rs gains max_cost_per_ingot / max_cost_per_run; agent.rs checks accumulated usage.cost each turn (engine/mod.rs:130-141 already sums it), emits a Warning event at 80% and cracks with reason 'budget' at 100%. Unattended runs need a runaway-spend guard.
  _evidence: Claude Code src/QueryEngine.ts:971-1002; src/components/CostThresholdDialog.tsx_
- [x] **7. Add a compaction circuit breaker after 3 failed shrinks** (S)
  engine/agent.rs chat_shrinking + engine/compact.rs: count consecutive compactions that fail to get below budget; after 3, crack the ingot with a clear 'context irrecoverable' error instead of halving budgets forever, and record it in the JSONL event stream.
  _evidence: Claude Code src/services/compact/autoCompact.ts:57-70,257-349; src/query.ts:1292-1296_
- [x] **8. Gate read_file on byte size before reading** (S)
  engine/tools.rs read_file: stat first; if len > 256 KB and no offset/limit given, return an error naming the size and instructing offset/limit use — the error costs ~100 bytes vs ~25K tokens of truncated content.
  _evidence: Claude Code src/tools/FileReadTool/limits.ts:1-18; src/utils/file.ts:48_
- [x] **9. Coerce stringly-typed tool args ("true", "5") before parsing** (S)
  engine/tools.rs arg helpers: as_bool silently drops a string "true" to false today. Add opt_bool/opt_u64 helpers accepting Value::String("true"/"false") and numeric strings, used by edit_file/bash/read_file. Naive truthiness is wrong ('false' -> true).
  _evidence: Claude Code src/utils/semanticBoolean.ts; src/utils/semanticNumber.ts_
- [x] **10. Interpret per-command exit codes (grep 1 = no matches, not error)** (S)
  engine/tools.rs run_shell: before appending '(exit N)', consult a static map keyed on the last pipeline segment's argv[0] (grep/rg 1='no matches', diff 1='files differ', test 1='condition false'); only >=2 is a real error. Also fixes proofs using grep/diff.
  _evidence: Claude Code src/tools/BashTool/commandSemantics.ts_
- [x] **11. Default-deny destructive bash commands with a confirm override** (S)
  engine/tools.rs bash: port the regex table (git reset --hard, push --force, clean -f, rm -rf, --no-verify, DROP TABLE, terraform destroy...); since slag forges unattended, refuse matches with the warning as the tool error plus a JSONL event; config flag to relax per crucible. Merges the advisory-annotation variant.
  _evidence: Claude Code src/tools/BashTool/destructiveCommandWarning.ts:12-102_
- [x] **12. Add a glob tool with 100-file cap and truncation notice** (S)
  engine/tools.rs: new `glob` tool (globset or `rg --files -g`), sandbox-resolved root, 100-file cap with '(Results are truncated...)' notice, paths relative to anvil root, corrected-path suggestion on missing dirs. Stops the smith burning bash calls on `find`.
  _evidence: Claude Code src/tools/GlobTool/GlobTool.ts:154-197_
- [x] **13. Warn edit_file about the read_file line-number prefix** (S)
  engine/tools.rs edit_file spec: slag's read_file emits 'LINENUM|CONTENT' but edit_file never mentions it. Add: never include the prefix in old_string/new_string; match content after the | byte-for-byte. Kills the #1 cause of failed exact-match edits.
  _evidence: Claude Code src/tools/FileEditTool/prompt.ts (getDefaultEditDescription)_
- [x] **14. Add numeric length anchors to the smith prompt** (S)
  engine/prompt.rs stable band: smiths are headless, inter-tool commentary is pure waste. Add '≤25 words between tool calls; finish summary ≤120 words'. Measured ~1.2% output-token reduction vs qualitative 'be concise'.
  _evidence: Claude Code src/constants/prompts.ts:529-537_
- [x] **15. Add the faithful-reporting rule to guard the proof gate** (S)
  engine/prompt.rs '## Rules' + finish tool description in engine/tools.rs: 'report outcomes faithfully; never claim tests pass when output shows failures; never weaken checks to manufacture green'. Pair with 'never modify the proof command or its tests unless the ingot asks'. Written against a measured 29-30% false-claim rate.
  _evidence: Claude Code src/constants/prompts.ts:237-242_
- [x] **16. Head-keep bash truncation with '[N lines truncated]' counts** (S)
  engine/tools.rs truncate_tail: switch to head-keep (or head 20% + tail 80% since build errors print last) and report the hidden line count instead of bytes; cap from SLAG_BASH_OUTPUT_CAP with a hard ceiling in config.rs.
  _evidence: Claude Code src/tools/BashTool/utils.ts:133-165; src/utils/shell/outputLimits.ts_

## Wave 2

- [ ] **17. Require read-before-edit and refuse stale edits via mtime** (M)
  engine/tools.rs: ToolBox read_state: Mutex<HashMap<PathBuf,(mtime, checksum)>> updated by read/write/edit; edit_file and write_file on existing files error if never read or mtime moved since. State the enforcement in both tool descriptions ('this tool will error if...'). Protects proof-gated ingots from clobbering parallel-anvil changes.
  _evidence: Claude Code src/tools/FileEditTool/FileEditTool.ts:275-306; src/tools/FileWriteTool/FileWriteTool.ts:190-216_
- [x] **18. Return a 'file unchanged' stub on repeated identical reads** (M)
  engine/tools.rs read_file: keep per-session map path -> (mtime, hash, offset/limit); on identical re-read return 'File unchanged since last read — refer to the earlier tool_result' instead of contents. Big token win in long forge loops.
  _evidence: Claude Code src/tools/FileReadTool/prompt.ts (FILE_UNCHANGED_STUB); FileReadTool.ts:686-691_
- [ ] **19. Spill oversized tool results to disk with preview + path** (M)
  engine/tools.rs run_shell + engine/agent.rs: when merged output exceeds BASH_OUTPUT_CAP, write full output to logs/tool-results/<call_id>.txt inside the anvil, return head preview + 'full output saved to <relative path>' so the smith can grep it. Also apply to grep overflow.
  _evidence: Claude Code src/utils/toolResultStorage.ts; src/Tool.ts:466_
- [ ] **20. Upgrade grep: output modes, context lines, head_limit, relative paths** (M)
  engine/tools.rs grep: add output_mode (default files_with_matches via rg -l), -A/-B/-C context, -i, glob filter, and head_limit passed through to rg; relativize paths to the anvil root instead of absolute resolved paths.
  _evidence: Claude Code src/tools/GrepTool/GrepTool.ts:52-107,316-365_
- [ ] **21. Trigger compaction on usage-anchored token counts, not chars** (M)
  engine/agent.rs: provider.rs already parses OpenRouter usage; store last-response prompt/total tokens on the session, add chars/4 estimates only for messages appended since (never cumulative sums — they double-count), and drive compact() from tokens.
  _evidence: Claude Code src/utils/tokens.ts:200-261; services/compact/microCompact.ts:164-205_
- [ ] **22. Derive context budget from model window minus output reserve** (M)
  engine/agent.rs char_budget_from_env is a fixed constant; fetch context_length per model from OpenRouter /models (provider.rs) and compute budget = window − output_reserve − compact_buffer, keeping SLAG_CHAR_BUDGET as override.
  _evidence: Claude Code src/services/compact/autoCompact.ts:28-91_
- [ ] **23. Prune at API-round boundaries to keep tool pairing intact** (M)
  engine/compact.rs: add a round-grouping helper over ChatMessage (assistant-with-tool_calls starts a group, its tool results belong to it) and make all drops operate on whole rounds — OpenAI-format providers on OpenRouter 400 on orphan tool messages.
  _evidence: Claude Code src/services/compact/grouping.ts:22-63_
- [ ] **24. Stub only replayable read-only tool results when pruning** (S)
  engine/compact.rs prunable(): key stub-eligibility on tool name (read/bash/grep are re-runnable; keep edit results and proof outputs), keep the last result per file path, and floor keepRecent at 1 so the model never loses all working context.
  _evidence: Claude Code src/services/compact/microCompact.ts:40-50,456-463_
- [ ] **25. Standardize a <system-reminder> envelope for all injections** (M)
  engine/prompt.rs + agent.rs: one wrap_reminder() helper for steer injections, proof-failure notices, workspace refreshes, truncation/staleness notes, with the 'may or may not be relevant' disclaimer; declare the contract once in the stable band; coalesce consecutive reminders. Merges the tool-result advisory and injected-context variants.
  _evidence: Claude Code src/constants/prompts.ts:131-134,186-197; src/utils/messages.ts:3098; src/utils/api.ts:460-472_
- [ ] **26. Add a git safety protocol to the bash spec** (S)
  engine/tools.rs bash spec / engine/prompt.rs: never --no-verify; never amend after a pre-commit hook failure (the commit did NOT happen — amend destroys the previous commit); stage specific files over `git add -A`; heredoc commit messages; no -i flags. Anvils commit in worktrees, so a botched amend poisons the merge in anvil/worktree.rs.
  _evidence: Claude Code src/tools/BashTool/prompt.ts:88-140_
- [ ] **27. Add the minimal-uniqueness hint for old_string** (S)
  engine/tools.rs edit_file description: 'use the smallest old_string that is clearly unique — 2-4 adjacent lines usually suffice' plus the not-unique failure note (add context or replace_all). Fewer whitespace-drift mismatches feeding the fuzzy ladder.
  _evidence: Claude Code src/tools/FileEditTool/prompt.ts (minimalUniquenessHint)_
- [ ] **28. Return precise warnings for empty files and past-EOF offsets** (S)
  engine/tools.rs read_file: on empty content return the 'file exists but contents are empty' reminder; on offset past EOF return 'file has M lines' so the model corrects its next call instead of guessing.
  _evidence: Claude Code src/tools/FileReadTool/FileReadTool.ts:703-708_
- [ ] **29. Teach parallel-vs-sequential command batching in the bash spec** (S)
  engine/tools.rs bash spec: independent commands -> multiple tool calls in ONE message; dependent -> chain with '&&'; ';' only when earlier failures don't matter; no newline-separated commands. agent.rs already dispatches parallel tool calls — this wording makes the dispatcher get used.
  _evidence: Claude Code src/tools/BashTool/prompt.ts (multipleCommandsSubitems)_
- [ ] **30. Add anti-gold-plating code-style rules to the smith prompt** (S)
  engine/prompt.rs stable band: no features/refactors/comments beyond the ask; validate only at system boundaries; 'three similar lines beat a premature abstraction'; don't remove existing comments. Smaller diffs mean faster duel judging and fewer proof-adjacent regressions.
  _evidence: Claude Code src/constants/prompts.ts:200-214_
- [ ] **31. Append 'did you mean' path suggestions on file-not-found** (S)
  engine/tools.rs resolve()/read_file errors: on 'cannot read' or 'escapes workspace', append the anvil root and probe for the same basename/relative tail under the root (single walkdir pass) as a hint. Cuts a whole retry turn per wrong-path guess.
  _evidence: Claude Code src/utils/file.ts:213,228-263; src/tools/GlobTool/GlobTool.ts:109-119_
- [ ] **32. Raise the retry budget and make it env-overridable** (S)
  engine/provider.rs: MAX_ATTEMPTS=3 becomes config/env driven (SLAG_MAX_RETRIES, default ~8-10) now that backoff is exponential; three attempts over 1.25s is far too brittle for overnight forge runs.
  _evidence: Claude Code src/services/api/withRetry.ts:52,789-797_
- [ ] **33. Emit structured api_retry heartbeat events** (M)
  engine/events.rs: add EngineEvent::ApiRetry{attempt,max,delay_ms,status}; provider.rs takes an optional EventTx to emit before each sleep; dashboard.rs renders a countdown on the anvil row instead of a silent hang.
  _evidence: Claude Code src/services/api/withRetry.ts:466-511; src/QueryEngine.ts:943-955_
- [ ] **34. Compute pricing locally when OpenRouter omits cost, flag estimates** (M)
  engine/provider.rs: when usage.cost is None, compute from a pricing map fetched once from OpenRouter GET /models and cached in the config dir; mark Usage estimated:bool so dashboard/assay print '~$0.0123 (est)'.
  _evidence: Claude Code src/utils/modelCost.ts:104-163; src/cost-tracker.ts:228-234_
- [ ] **35. Split the cost ledger per model and per role** (M)
  dashboard.rs: state.totals becomes HashMap<(model, role), Usage> fed by EngineEvent::Tokens (add model+role fields in engine/mod.rs); pipeline/assay.rs prints smith vs judge vs founder vs surveyor rows. Duel/judge spend becomes visible.
  _evidence: Claude Code src/cost-tracker.ts:181-226,250-276,304-322_
- [ ] **36. Show OpenRouter credit balance beside session spend** (S)
  Add `slag cost` (cli.rs): current run ledger plus OpenRouter GET /api/v1/credits via provider.rs's HTTP client — 'session $0.42 · account $18.31 remaining'; warn at forge start when balance < configured floor.
  _evidence: Claude Code src/commands/usage/usage.tsx; src/commands/extra-usage/extra-usage-core.ts:33-51_
- [ ] **37. Add an <env> block with model identity and knowledge cutoff** (S)
  engine/prompt.rs workspace_snapshot(): <env> sub-block with platform/shell/OS via std::env + os_info, 'You are powered by <OpenRouter slug>', and a small cutoff table for routed model families. Stable within a session, so cache-safe in the context band.
  _evidence: Claude Code src/constants/prompts.ts:606-756_
- [ ] **38. Register central cleanup handlers plus a panic hook** (S)
  New shutdown.rs: static registry of boxed cleanups (flush JSONL sink, save crucible under CRUCIBLE_LOCK, restore ratatui terminal) invoked from a ctrl-c handler and a panic hook in main.rs so a panic never leaves the terminal in raw mode or drops buffered events.
  _evidence: Claude Code src/utils/cleanupRegistry.ts; src/utils/gracefulShutdown.ts_
- [ ] **39. Require double-press Ctrl-C with a 'press again to cancel' hint** (S)
  dashboard.rs handle_key: first Ctrl-C sets pending_cancel: Option<Instant> and flashes a hint; second press within 800ms stores CancelFlag. Protects long forges from an accidental cancel.
  _evidence: Claude Code hooks/useDoublePress.ts (DOUBLE_PRESS_TIMEOUT_MS=800)_
- [ ] **40. Tint stalled anvils red after seconds without tokens** (S)
  dashboard.rs: store last-token Instant per forging IngotRow (updated on Tokens/ToolResult); tint the row yellow after 15s of silence and red after 60s with a '(stalled Ns)' suffix. Pure display change, instant 'is it hung?' answer.
  _evidence: Claude Code components/Spinner/useStalledAnimation.ts (3000ms threshold)_
- [ ] **41. Show queued steer messages as a capped persistent list** (S)
  dashboard.rs: replace the 1.5s 'steer queued' flash with a persistent dim list of SteerQueue contents above the input (cap 3 + '+N more'), removing entries when EngineEvent::Steer confirms delivery.
  _evidence: Claude Code components/PromptInput/PromptInputQueuedCommands.tsx_

## Wave 3

- [ ] **42. Add LLM summarization compaction with the 9-section template** (L)
  engine/compact.rs stage two: when stub-pruning cannot reach budget, call the provider with a Rust port of BASE_COMPACT_PROMPT (9 fixed sections, <analysis> scratchpad stripped before reuse), replace the head with one user message containing the summary, keep the recent tail. Wire into agent.rs chat_shrinking before giving up.
  _evidence: Claude Code src/services/compact/prompt.ts (BASE_COMPACT_PROMPT); services/compact/compact.ts:387-520_
- [ ] **43. Lead text-only LLM calls with a consequence-first NO-TOOLS preamble** (S)
  engine/judge.rs SYSTEM_PROMPT and the compact summarizer: send with no tools in the request AND lead with 'Respond with TEXT/JSON ONLY — tool calls will be REJECTED and waste your only turn' plus a matching trailer. Measured fallback rate drop 2.79% -> 0.01%; reduces slag's malformed-JSON judge retries.
  _evidence: Claude Code src/services/compact/prompt.ts:12-26,269-272_
- [ ] **44. Wrap compaction summaries in a resume-silently continuation message** (S)
  engine/compact.rs summarizer path: deliver the summary as 'continued from a previous conversation that ran out of context — pick up the last task as if the break never happened, do not acknowledge the summary', with a pointer to slag's JSONL event log for exact earlier output.
  _evidence: Claude Code src/services/compact/prompt.ts:337-374_
- [ ] **45. Retry an overflowing compaction call by dropping oldest rounds** (S)
  engine/compact.rs summarizer path: on context-overflow from the summary call (reuse is_context_overflow in agent.rs), drop oldest rounds sized to the gap (fallback 20%) and retry ≤3 before failing.
  _evidence: Claude Code src/services/compact/compact.ts:227-292,449-490_
- [ ] **46. Track read-file state and re-inject top files after compaction** (M)
  engine/tools.rs records path+timestamp on every read/edit into a per-session FileState map; engine/compact.rs summarizer path re-reads the 5 most recent files (capped) and appends them as a system-reminder after the summary, skipping files whose reads survive in the tail.
  _evidence: Claude Code src/services/compact/compact.ts:122-129,1399-1464_
- [ ] **47. Detect externally-changed files and inject diff reminders** (M)
  engine/agent.rs turn loop: stat tracked FileState files before each provider call; on mtime change inject '<system-reminder>file X was modified externally — this change was intentional, don't revert it: <diff snippet>'. Evict only on ENOENT. High leverage with MAX_ANVILS=3 parallel smiths touching shared files.
  _evidence: Claude Code src/utils/attachments.ts:2063-2161; src/utils/messages.ts (case 'edited_text_file')_
- [ ] **48. Switch to a fallback model after consecutive capacity errors** (M)
  config.rs gains fallback_model; agent.rs catches a Capacity-class error after N attempts and retries the turn on the fallback — or passes OpenRouter's native models:[primary,fallback] routing array in provider.rs build_body, which is nearly free; emit an event so the dashboard shows the switch.
  _evidence: Claude Code src/services/api/withRetry.ts:326-364; src/query.ts:893-951_
- [ ] **49. Add unattended persistent-retry mode with chunked heartbeat sleeps** (M)
  SLAG_UNATTENDED_RETRY config: 429/529 retry indefinitely with backoff capped at 5min and a ceiling; chunk long sleeps into 30s slices each emitting an ApiRetry heartbeat so dashboard and logs stay alive; wait until rate-limit reset timestamps instead of polling. Slag IS an unattended orchestrator.
  _evidence: Claude Code src/services/api/withRetry.ts:91-104,433-506,814-822_
- [ ] **50. Give side-LLM calls a minimal retry policy** (S)
  engine/provider.rs: add RetryPolicy(attempts, persistent?) on ChatRequest; forge strikes get full retries while judge.rs duel judging, recipe suggestions, and assay summaries get 0-1 attempts, so a capacity event never multiplies load across MAX_ANVILS workers.
  _evidence: Claude Code src/services/api/withRetry.ts:57-89,316-324_
- [ ] **51. Recover from output-token caps: silent escalation then continuation** (S)
  engine/agent.rs: the single 'continued' flag on FinishReason::Length (agent.rs:171) becomes: retry once with raised max_tokens, then up to 3 continuation nudges using CC's proven 'resume directly — no apology, no recap' text, then fail with a clear reason.
  _evidence: Claude Code src/query.ts:164,1185-1256_
- [ ] **52. Parse context-overflow 400s and shrink max_tokens before compacting** (M)
  engine/agent.rs (context-error substrings at agent.rs:47-48): parse 'A + B > C' numbers from the provider 400 body, retry once with max_tokens = limit - input - buffer; fall through to compact.rs only if that fails — cheaper than compaction when only the output budget overflowed.
  _evidence: Claude Code src/services/api/withRetry.ts:384-427,550-595_
- [ ] **53. Make request timeouts configurable and rebuild stale connections** (S)
  engine/provider.rs: REQUEST_TIMEOUT_SECS=600 becomes SLAG_API_TIMEOUT_MS (default ~300s); on reqwest is_connect/reset errors, rebuild the Client with pool_max_idle_per_host(0) for the retry. The 90s idle-watchdog half applies when slag adds SSE streaming.
  _evidence: Claude Code src/services/api/claude.ts:795-811,1868-1928; withRetry.ts:112-118,218-230_
- [ ] **54. Replace string provider errors with a typed taxonomy** (M)
  error.rs: SlagError::Provider(String) becomes {status, category, retryable, excerpt} classified once in provider.rs; events.rs includes the category in JSONL; dashboard shows 'credit balance low' or 'invalid key' instead of a raw body excerpt.
  _evidence: Claude Code src/services/api/errors.ts:1040-1182_
- [ ] **55. Discard partial turn state before any retry** (S)
  engine/agent.rs: on fallback-model or compact-retry paths, rebuild the outgoing message vec from last committed state instead of appending; audit run_with_compaction so a failed attempt's partial assistant/tool messages never leak into the retried request; mirror in duel.rs round retries.
  _evidence: Claude Code src/query.ts:712-741,899-919_
- [ ] **56. Thread a CancellationToken with reasons into tools and provider** (M)
  engine/agent.rs: CancelFlag (checked only at turn boundaries) becomes a tokio CancellationToken threaded into ToolCtx and provider chat futures via select!, with reason enum (UserAbort vs SteerInterrupt) — ctrl-C kills a 10-minute bash proof immediately, and steer-interrupts skip the error path.
  _evidence: Claude Code src/utils/handlePromptSubmit.ts:331; src/query.ts:1044-1051,1496-1515_
- [ ] **57. Teach the founder to brief ingots like zero-context colleagues** (M)
  pipeline/founder.rs: ingots ARE zero-context subagent prompts. Add the rules — goal + why + what's ruled out + concrete file paths; lookups get exact commands, investigations get the question; never delegate understanding. Directly raises first-heat pass rate.
  _evidence: Claude Code src/tools/AgentTool/prompt.ts (writingThePromptSection)_
- [ ] **58. Use per-filetype bytes-per-token ratios in estimates** (S)
  engine/tools.rs truncation budgets + compact.rs estimator: 2 bytes/token for .json/.jsonl (dense punctuation tokenizes worse), 4 otherwise; flat-cost any base64 image content instead of counting chars. Underestimates let oversized results slip in and trigger compaction too late.
  _evidence: Claude Code src/services/tokenEstimation.ts:215-224,400-423_
- [ ] **59. Label instruction-file provenance and warn on oversized files** (S)
  engine/prompt.rs stable band: when loading AGENTS.md/CLAUDE.md/recipes, emit 'Contents of <path> (project instructions, checked in):' labels under an OVERRIDE header, and log a dashboard warning when any single file exceeds a char cap.
  _evidence: Claude Code src/utils/claudemd.ts:89-92,1132-1204_
- [ ] **60. Annotate injected blueprints and recipes with human-readable age** (S)
  engine/prompt.rs + engine/recipes.rs: stat mtime when injecting BLUEPRINT.md, PROGRESS.md history, or recipes and prepend 'written N days ago — verify against current code' for stale ones. Models are poor at date arithmetic; raw ISO timestamps don't trigger staleness reasoning.
  _evidence: Claude Code src/memdir/memoryAge.ts:1-53_
- [ ] **61. Add a live spinner status line: verb, elapsed, tokens, rate** (M)
  progress.rs (stream mode) + dashboard.rs bottom bar: fold EngineEvent::Tokens deltas plus a turn-start Instant into '⚒ Forging… (12s · 4.1k tok · 38 tok/s · esc to interrupt)'; metallurgical verb list in tui.rs; guard the rate readout (>5s elapsed, >2000 tok) against noisy early samples.
  _evidence: Claude Code components/Spinner.tsx:216-279; constants/spinnerVerbs.ts_
- [ ] **62. Render word-level diffs with a 40% change-ratio fallback** (M)
  New src/render/diff.rs using the `similar` crate (word granularity + ratio check, fall back to full-line coloring above 40% change), used by dashboard.rs expanded view and progress.rs when showing anvil edit results — slag currently prints raw previews only.
  _evidence: Claude Code components/StructuredDiff/Fallback.tsx (CHANGE_THRESHOLD=0.4)_
- [ ] **63. Collapse tool results to one-liners with counts, ctrl-o to expand** (M)
  engine/events.rs: add line/byte counts and duration to ToolResult events; dashboard.rs feed shows '✓ read_file (43 lines · 0.3s)' and a ctrl-o toggle swaps to full previews; one hint in the HINT bar, not per line.
  _evidence: Claude Code components/messages/CollapsedReadSearchContent.tsx:276-290_
- [ ] **64. Show a context-percent gauge with compaction-buffer accounting** (M)
  engine/compact.rs knows the window and trigger threshold: emit a ContextGauge event; dashboard.rs bottom bar appends 'ctx 48%' coloring WARM→HOT near the compaction trigger; also emit the percentage in JSONL for headless runs; a 'ctx' steer keyword prints the breakdown.
  _evidence: Claude Code src/services/compact/autoCompact.ts:93-145; components/ContextVisualization.tsx_
- [ ] **65. Send terminal notifications on finish/error, gated on idleness** (M)
  New tui.rs notify() writing BEL + OSC 9/99/777 by TERM_PROGRAM detection, called from pipeline/forge.rs on assay completion and Error/Finish events; dashboard.rs tracks last-keypress Instant and only notifies after 6s of user idleness.
  _evidence: Claude Code ink/useTerminalNotification.ts; hooks/useNotifyAfterTimeout.ts_
- [ ] **66. Substitute $ARGUMENTS, $0..$n, and named args into recipes** (S)
  recipes.rs: `slag recipe run <name> -- args` and in-conversation invocation substitute $ARGUMENTS/$0..$n/named placeholders into RECIPE.md before injection (shell-words crate); append 'ARGUMENTS: ...' when no placeholder exists. Turns static recipes into parameterized ones.
  _evidence: Claude Code utils/argumentSubstitution.ts_

## Wave 4

- [ ] **67. Persist per-ingot transcripts as JSONL and resume mid-ingot** (L)
  New engine/transcript.rs: agent.rs appends each ChatMessage + tool outcome to logs/transcripts/<ingot>-h<heat>.jsonl; forge.rs, on finding a Molten ingot with a transcript at restart, reloads messages and resumes the agentic loop at the recorded turn instead of resetting to Ore and burning a heat. Emit a compact_boundary line when compaction fires so resume loads only post-boundary context.
  _evidence: Claude Code src/utils/sessionStorage.ts (recordTranscript, adoptResumedSessionFile); sessionStoragePortable.ts:480-735_
- [ ] **68. Checkpoint files before edits and rewind on failed heats** (L)
  New anvil/checkpoint.rs hooked into engine/tools.rs write/edit dispatch: back up each file before first modification per attempt ({hash}@vN, mtime+size dedupe), snapshot at attempt start; on proof failure + heat retry (forge.rs) rewind the workspace to the attempt-start snapshot so retries start clean; add `slag rewind` to cli.rs. Outsized win for proof-gated retries.
  _evidence: Claude Code src/utils/fileHistory.ts:86-347,748_
- [ ] **69. Add a lifecycle hook engine with exit-code protocol** (L)
  New engine/hooks.rs: config.rs [hooks] table keyed by event enum (pre_tool, post_tool, tool_error, stop, session_start, pre_compact, ingot_forged, ingot_cracked). agent.rs calls hooks around tool dispatch; forge.rs fires ingot/session events. Exit 0 stdout becomes model-visible context; exit 2 blocks and feeds stderr to the smith.
  _evidence: Claude Code utils/hooks.ts:747,1453,2648; entrypoints/sdk/coreTypes.ts:25_
- [ ] **70. Match hooks by exact name, pipe-alternation, regex, or wildcard** (S)
  hooks.rs matcher: three-tier match (exact, 'bash|edit' alternation via the /^[a-zA-Z0-9_|]+$/ fast path, regex fallback) against slag tool names; empty or '*' matches all; invalid regex logs and skips instead of panicking.
  _evidence: Claude Code utils/hooks.ts:1338-1380_
- [ ] **71. Support per-hook timeout, once, async, and asyncRewake** (M)
  hooks.rs runs each hook under tokio::time::timeout with per-hook override; `once` removes from the session list after first run; async backgrounds without blocking; asyncRewake maps onto slag's steer channel — on exit 2 push a steer message into the smith's queue.
  _evidence: Claude Code schemas/hooks.ts; utils/hooks.ts:166-268_
- [ ] **72. Snapshot hook config at session start with a kill-switch** (S)
  hooks.rs loads config once in Engine::new and stores an immutable snapshot; config.rs adds disable_all_hooks. Guard rail matters because forge.rs lets smiths edit the workspace — a smith writing slag.toml must not register a hook into its own running session.
  _evidence: Claude Code utils/hooks/hooksConfigSnapshot.ts; utils/hooks.ts:286-300_
- [ ] **73. Filter hooks with cheap in-process `if` preconditions** (S)
  hooks.rs: `if = "bash(cargo *)"` glob evaluated against serialized tool args before process::Command::spawn, so a formatter hook on edit never forks for unrelated calls. Direct win where MAX_ANVILS parallel smiths would multiply useless hook spawns.
  _evidence: Claude Code schemas/hooks.ts (IfConditionSchema); utils/hooks.ts:1386-1440_
- [ ] **74. Add LLM prompt hooks and agentic verifier hooks** (M)
  hooks.rs kind Prompt{prompt, model} routed through engine/judge.rs on a cheap OpenRouter model; kind Agent runs a one-ingot smith. Natural upgrade for proof.rs: ingots whose acceptance can't be a shell command get `:proof-judge "..."` verified by a prompt hook.
  _evidence: Claude Code schemas/hooks.ts (PromptHookSchema, AgentHookSchema); utils/hooks.ts:2224-2295_
- [ ] **75. Let PreToolUse hooks rewrite tool input and inject context** (M)
  hooks.rs parses optional JSON from hook stdout; tools.rs applies updated_input before executing and appends additional_context to the tool result. Enables guard hooks that rewrite dangerous commands or inject lint output after edits without a model round-trip.
  _evidence: Claude Code types/hooks.ts:70-107; services/tools/toolHooks.ts:348-452_
- [ ] **76. Add HTTP hooks with env-var-allowlisted headers** (S)
  hooks.rs HTTP kind using the reqwest stack from provider.rs: POST event JSON to a URL; headers interpolate only variables named in allowedEnvVars (everything else empty) so config files can't exfiltrate secrets. Gives CI notification on ingot_cracked.
  _evidence: Claude Code schemas/hooks.ts (HttpHookSchema); utils/hooks.ts:2295-2330_
- [ ] **77. Document and instrument hooks: metadata table plus progress events** (S)
  hooks.rs exposes a static EVENTS table (event, summary, stdin/exit-code contract) printed by `slag hooks list`; emit hook_started/hook_finished JSONL events with name, exit code, duration_ms; render optional status_message in the smith status line so slow hooks don't look like a hung smith.
  _evidence: Claude Code utils/hooks/hooksConfigManager.ts:26-140; utils/hooks.ts:2104-2110,2240-2250_
- [ ] **78. Execute inline !`cmd` shell spans in recipe bodies** (M)
  recipes.rs expansion pass runs !`cmd` spans and ```! blocks through the same bash executor tools.rs uses (inheriting sandbox/allowlist) and splices stdout. Recipes can embed live state: !`git status --porcelain`. Build the output string manually — never regex-replace with untrusted text.
  _evidence: Claude Code utils/promptShellExecution.ts_
- [ ] **79. Emit a `slag status --json` contract for external consumers** (M)
  cli.rs: read the live JSONL event log plus persisted session costs and print one JSON object: run id, ingots by status, spend, tokens, context %, active anvils. Lets tmux statuslines, CI, and the website poll a forge without scraping the TUI.
  _evidence: Claude Code src/components/StatusLine.tsx:46-98_
- [ ] **80. Make JSONL readers crash-tolerant: skip bad lines and truncated tails** (S)
  engine/transcript.rs and any logs/ reader: parse per-line with serde, warn once and skip malformed lines, treat a non-newline-terminated last line as a partial write to drop — mirrors the guard crucible.rs already has for ingot lines.
  _evidence: Claude Code src/history.ts:106-143; src/utils/sessionStoragePortable.ts:735_
- [ ] **81. Append run metadata as typed entries in the run JSONL** (S)
  events.rs sink gains a metadata entry type: forge start appends {run_id, git_branch, model, flux_profile, crucible_hash}; assay appends the final verdict — one self-describing log file per run, no sidecar files for the runs lister.
  _evidence: Claude Code src/utils/sessionStorage.ts:2572-2815_
- [ ] **82. List past runs from 64KB head+tail windows, never full parses** (M)
  New `slag runs` subcommand + dashboard run picker: stat + head/tail window reads per logs/*.jsonl with a no-parse '"key":"value"' scanner to pull start time, blueprint name, last event, and pass/fail — instant even with megabyte-scale event logs.
  _evidence: Claude Code src/utils/listSessionsImpl.ts:83; sessionStoragePortable.ts (LITE_READ_BUF_SIZE)_
- [ ] **83. Register live forges in a PID registry with `slag ps`** (S)
  main.rs writes ~/.slag/sessions/<pid>.json (pid, cwd, run id, phase, started_at), removed via the cleanup registry; `slag ps` in cli.rs lists live forges (pid-liveness checked) — and forge.rs can refuse to start a second forge on the same crucible directory.
  _evidence: Claude Code src/utils/concurrentSessions.ts:60_
- [ ] **84. Print a copy-pasteable resume hint on interrupted exit** (S)
  forge.rs / main.rs: when exiting via Ctrl-C or provider failure with ore/molten ingots remaining, print 'resume with: slag forge' plus counts (N ore, M molten reset) after the ratatui teardown, so the operator knows the run is resumable.
  _evidence: Claude Code src/utils/gracefulShutdown.ts:144_
- [ ] **85. Persist steer history with buffered lockfile flush and recall** (M)
  dashboard.rs steer input: persist submitted steers to ~/.slag/history.jsonl (project field = cwd) with lockfile + bounded retries, load newest-first deduped for up-arrow recall across runs; flush through the cleanup registry so Ctrl-C never loses the last steer.
  _evidence: Claude Code src/history.ts:190-329_
- [ ] **86. Persist and restore run cost state across resume** (S)
  Persist .slag/session-costs.json keyed by run id from dashboard.rs state on exit/crack; flux.rs reloads it when re-melting so assay reports whole-job spend, not just the last invocation.
  _evidence: Claude Code src/cost-tracker.ts:87-175; src/costHook.ts_
- [ ] **87. Split wall vs API vs tool duration, including retry overhead** (S)
  engine/agent.rs times each provider.chat() call (retries separately) and each ToolBox dispatch; assay.rs prints 'wall 4m12s · api 2m01s (retries +18s) · tools 1m40s'. Diagnoses whether slowness is model, retry storms, or proofs.
  _evidence: Claude Code src/cost-tracker.ts:11-21,71-80,238-242_
- [ ] **88. Track lines added/removed per ingot and per run** (S)
  engine/tools.rs edit/write handlers diff old vs new content and emit added/removed counts in ToolResult events; forge aggregates per ingot so assay shows churn per ingot and total.
  _evidence: Claude Code src/cost-tracker.ts:54-56,241_
- [ ] **89. Aggregate tool-error categories into the assay report** (S)
  forge.rs already sees ToolResult{ok:false} events: aggregate counts per tool name plus a coarse error class from the output prefix — 'tool errors: edit 7 (no-match 5), bash 2'. Directly surfaces when the fuzzy edit ladder is thrashing.
  _evidence: Claude Code src/commands/insights.ts:240-299_
- [ ] **90. Run idempotent startup migrations for model slugs and formats** (S)
  New migrations.rs called early in main.rs: rewrite deprecated OpenRouter model slugs in ~/.slag config and Slagfile (OpenRouter retires slugs regularly) and upgrade old crucible header formats — each migration a pure idempotent fn over config.rs types, no completion flags.
  _evidence: Claude Code src/migrations/migrateFennecToOpus.ts_
- [ ] **91. Export runs as Chrome/Perfetto trace files** (M)
  Add `--trace trace.json` to forge: a second event sink beside spawn_jsonl_sink (engine/events.rs:62) mapping IngotStart/ToolCallStart/ToolResult/IngotDone to Chrome trace-event B/E pairs, one lane per anvil, args carrying tokens+cost. Open in ui.perfetto.dev to see where parallel forges serialize.
  _evidence: Claude Code src/utils/telemetry/sessionTracing.ts:1-80; perfettoTracing.ts_
- [ ] **92. Report forge progress via OSC 9;4 with tmux passthrough** (S)
  progress.rs: forged/total ratio → osc_progress(pct) after each IngotDone, error state on crack, cleared in restore_terminal(); copy CC's terminal allowlist (exclude Windows Terminal). Route all OSC emissions through a single tui.rs write_osc() that wraps in DCS passthrough when $TMUX/$STY is set.
  _evidence: Claude Code ink/terminal.ts:17-57; ink/termio/osc.ts:428-444 (wrapForMultiplexer)_
- [ ] **93. Set the terminal title to live forge state, clear on exit** (S)
  tui.rs set_title() via OSC 0 (through the multiplexer wrapper): '⚒ slag 3/9 forging i4' updated on IngotStart/IngotDone in progress.rs; clear in dashboard.rs restore_terminal() and the panic hook.
  _evidence: Claude Code utils/gracefulShutdown.ts:122; ink/termio/osc.ts_
- [ ] **94. Enable DEC 2026 synchronized output for flicker-free draws** (M)
  dashboard.rs draw loop: wrap each frame in crossterm's BeginSynchronizedUpdate/EndSynchronizedUpdate when the terminal qualifies (port CC's allowlist + tmux exclusion into tui.rs); reuse the DA1-sentinel probe trick if slag later queries capabilities.
  _evidence: Claude Code ink/terminal.ts:67-104; ink/terminal-querier.ts:1-40_

## Wave 5

- [ ] **95. Build a command policy engine: split compounds, strip wrappers, prefix rules** (L)
  New engine/policy.rs consumed by tools.rs bash: config-driven allow/deny lists in slag.toml (deny = ["git push:*", "curl:*"]), compound-split (&&, ;, |) so every subcommand must pass, iterative wrapper/env-prefix stripping, fail-closed on backticks/$( ) command substitution, deny > ask > allow precedence. Real guard rail beyond the path sandbox.
  _evidence: Claude Code src/tools/BashTool/bashPermissions.ts:524-566; utils/permissions/shellRuleMatching.ts; dangerousPatterns.ts_
- [ ] **96. Classify provably read-only bash for concurrent scheduling** (L)
  engine/tools.rs path_access returns None for bash today, forcing every bash call to be treated as unscheduled. A trimmed classifier (ls/cat/rg/fd/git status/diff/log with safe-flag tables, excluding execution escapes like -exec and jq -f) lets agent.rs run read-only bash concurrently with readers and lets duels share a repo view safely.
  _evidence: Claude Code src/tools/BashTool/readOnlyValidation.ts:35-90,1509_
- [ ] **97. Support background bash with run_in_background and sleep guards** (L)
  engine/tools.rs + agent.rs: run_in_background arg spawns the process group detached with stdout redirected to logs/bg/<id>.log, returns id+path immediately, injects a completion note via the existing steer channel. The S-size subset — reject `sleep >2s` with 'background it instead' guidance — is worth doing alone.
  _evidence: Claude Code src/tools/BashTool/BashTool.tsx:241,525-530,610_
- [ ] **98. Grow recipe frontmatter: allowed-tools, model, fork context, paths gating** (L)
  recipes.rs Recipe struct gains allowed_tools, model, context (inline|fork), paths. `context: fork` spawns a sub-smith via smith/native.rs with only the recipe as its brief (forge.rs already runs parallel smiths); `paths` globs hide recipes until matching files are touched, cutting index tokens.
  _evidence: Claude Code skills/loadSkillsDir.ts:185-263; types/command.ts_
- [ ] **99. Add a minimal MCP stdio client to import external tools** (L)
  engine/tools.rs adds an mcp.rs adapter: spawn configured stdio servers (config.rs [mcp] table), initialize + tools/list at startup, expose each as a slag tool with mcp__server__tool naming; recipes' requires_tools works unchanged. Scope to stdio-only, no OAuth/HTTP transports.
  _evidence: Claude Code services/mcp/client.ts; services/mcp/config.ts_
- [ ] **100. Build `slag insights`: offline analytics over run logs with cached facets** (L)
  New cli.rs subcommand over logs/*.jsonl: deterministic stats (ingots forged/cracked, heats per ingot, cost per ingot, duel margins, tool errors) plus optional cheap-model facet extraction per run cached as logs/facets/<run>.json. Turns the slag heap into a learning loop across projects.
  _evidence: Claude Code src/commands/insights.ts:260-273,430,941-970_
