# Example: Building slag.dev

These files are real outputs from running slag to build its own website.

## What happened

```
slag "Build a terminal-UI website for slag.dev showing ingot task management with s-expressions"
```

slag ran its 4-phase pipeline:

1. **SURVEYOR** read `PRD.md` and produced `BLUEPRINT.md`
2. **FOUNDER** designed ingots and wrote them to `PLAN.md`
3. **FORGE** executed each ingot via Claude, ran proofs, retried failures
4. **ASSAY** wrote the final report to `PROGRESS.md`

## Files

| File | Phase | Contents |
|------|-------|----------|
| `PRD.md` | Input | Original requirements document |
| `BLUEPRINT.md` | Surveyor | Architecture analysis and forging sequence |
| `PLAN.md` | Founder | S-expression ingots with status, proofs, work |
| `PROGRESS.md` | Assay | Execution log with pass/fail results |
| `AGENTS.md` | Forge | Agent recipe documentation |
| `logs/` | Forge | Raw Claude output, prompts, and strike logs |

## Log naming convention

```
YYYYMMDD_HHMMSS_PHASE_DETAIL.log

20260127_094623_FLUX_i2_1.log   # Claude output for ingot i2, heat 1
20260127_094638_STRIKE_i3_1.log # Shell commands extracted from i3, heat 1
20260127_094656_ASSAY_i1_1.log  # Proof result for i1, heat 1
```
