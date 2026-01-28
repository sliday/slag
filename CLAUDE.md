# CLAUDE.md

## Project Overview

**slag** is a task orchestrator for AI-powered development using metallurgical metaphors. It breaks requirements into S-expression "ingots" and forges them via Claude with automatic retry and proof-based verification.

Two implementations:
1. **Rust binary** (`src/`) — Primary, async with tokio, parallel anvils via JoinSet
2. **Bash script** (`slag.sh`) — Legacy single-file orchestrator

Website at `website/` documents slag and is deployed to slag.dev via Cloudflare Pages.

## Build & Test

```bash
# Rust
cargo test --all
cargo clippy -- -D warnings
cargo run -- "Your commission"

# Bash tests
bash tests/test_slag.sh

# Website
cd website && npm install && npm run dev
cd website && npm run build
cd website && npx wrangler pages deploy dist --project-name=slag-dev
```

## Rust Module Structure

```
src/
  main.rs              Entry point, tokio runtime
  cli.rs               Clap argument parsing
  config.rs            Constants, paths, SmithConfig
  crucible.rs          PLAN.md file operations (load, save, replace, counts)
  error.rs             SlagError enum (thiserror)
  flux.rs              Claude CLI invocation, output capture
  proof.rs             Shell command verification (exit code checks)
  progress.rs          PROGRESS.md append, codebase pattern tracking
  tui.rs               Terminal UI (indicatif, crossterm colors)
  update.rs            Self-update via GitHub releases API
  sexp/
    mod.rs             Ingot struct, Status enum
    parser.rs          S-expression parser (hand-rolled, no deps)
    writer.rs          S-expression serializer
  smith/
    mod.rs             Smith trait (async forge interface)
    claude.rs          ClaudeSmith — real Claude CLI integration
    mock.rs            MockSmith — deterministic test responses
  pipeline/
    mod.rs             Pipeline orchestrator
    surveyor.rs        Phase 1: PRD analysis
    founder.rs         Phase 2: ingot generation
    forge.rs           Phase 3: parallel/sequential execution
    resmelt.rs         Phase 3b: failure recovery (rewrite/split/impossible)
    assay.rs           Phase 4: final report
  anvil/
    mod.rs             Parallel execution (tokio JoinSet)
    worktree.rs        Git worktree isolation per ingot
```

## Ingot S-Expression Format

```
(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5
       :proof "test -f file" :work "Task description")
```

| Field | Values |
|-------|--------|
| `:status` | ore / molten / forged / cracked |
| `:solo` | t (parallel) / nil (sequential) |
| `:grade` | 1-5 (>= 3 uses plan mode) |
| `:heat` | Current retry count |
| `:max` | Max retries |
| `:proof` | Shell command, exit 0 = pass |

## CI/CD

- **CI** (`.github/workflows/ci.yml`): Tests + clippy on ubuntu + macos, format check
- **Release** (`.github/workflows/release.yml`): Triggered by `v*` tags, builds 4 platform binaries, creates GitHub release

## Key Conventions

- S-expression parser is hand-rolled (no serde for ingots)
- Smith trait allows swapping Claude for mock in tests
- Crucible operations are atomic (load, modify, save)
- `website/public/slag.sh` is a copy of root `slag.sh` for download — sync manually
