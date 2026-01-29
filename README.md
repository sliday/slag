# slag

[slag.dev](https://slag.dev)

**Smelt ideas, skim the bugs, forge the product.**

A task orchestrator for AI-powered development. Give it a product requirement, and it breaks it into verifiable tasks, executes them via Claude, and proves each one passed before moving on.

![slag-promo](https://github.com/user-attachments/assets/d12def06-6eab-4236-9634-bbbd09be6683)

## Install

**Binary** (recommended):
```bash
curl -sSf https://slag.dev/install.sh | sh
```

**Bash version** (no build required):
```bash
curl -fsSL https://slag.dev/slag.sh -o /usr/local/bin/slag && chmod +x /usr/local/bin/slag
```

**From source**:
```bash
cargo install --git https://github.com/sliday/slag
```

## Quick start

```bash
# Write your requirements
cat > PRD.md << 'EOF'
Build a REST API with user authentication, rate limiting,
and PostgreSQL storage. Include health check endpoint.
EOF

# Forge it
slag "Build the REST API from PRD.md"
```

slag reads `PRD.md`, analyzes it, designs tasks, executes them, and proves each one works.

## Usage

```
slag [OPTIONS] [COMMISSION]... [COMMAND]
```

**Commands:**

| Command | Description |
|---------|-------------|
| `slag "Build X from PRD.md"` | Start a new forge from a commission |
| `slag status` | Show crucible state (ingot counts and progress) |
| `slag resume` | Resume an existing forge |
| `slag update` | Self-update to latest release |

**Options:**

| Flag | Default | Description |
|------|---------|-------------|
| `--worktree` | off | Enable branch-per-ingot worktree isolation with master review |
| `--anvils N` | 3 | Max parallel anvil workers |
| `--skip-review` | off | Skip the master review phase (legacy behavior) |
| `--keep-branches` | off | Don't delete branches after review |
| `--ci-only` | off | Run CI checks but skip AI review |
| `--review-all` | off | Review even if CI fails |
| `--retry N` | 3 | Max retry cycles when ingots crack (0 = no retry) |

## Progress display

slag shows emoji progress in the terminal:

```
[ ✅3  🔥1  🧱5 ] 37%
```

| Emoji | Status | Meaning |
|-------|--------|---------|
| 🧱 | queued | Ingot is ore, waiting to be forged |
| 🔥 | forging | Ingot is molten, currently being worked |
| ✅ | done | Ingot is forged, proof passed |
| ❌ | failed | Ingot cracked after exhausting all heats |

The percentage shows overall progress: forged ingots / total ingots.

## Language

slag uses metallurgical vocabulary. Here's the dictionary.

### Nouns

| Term | What it is | File/location |
|------|-----------|---------------|
| **Ore** | Raw requirements; the starting material | `PRD.md` |
| **Ingot** | A single task encoded as an S-expression | One line in `PLAN.md` |
| **Crucible** | The file holding all ingots | `PLAN.md` |
| **Blueprint** | Architecture analysis and forging plan | `BLUEPRINT.md` |
| **Anvil** | A parallel execution slot (background process) | In-memory |
| **Smith** | The AI agent that does the work (Claude) | Claude CLI invocation |
| **Slag heap** | Debug logs dumped during forging | `logs/` directory |
| **Heat** | One attempt at forging an ingot (retry count) | `:heat` field |
| **Grade** | Complexity rating (1-5); high grade = plan mode | `:grade` field |
| **Proof** | Shell command that verifies the work (exit 0 = pass) | `:proof` field |
| **Skill** | Tool configuration for the smith (web, default) | `:skill` field |
| **Temper bar** | Progress visualization in the terminal | TUI output |
| **Sparks** | Animated spinner shown during work | TUI output |

### Verbs

| Term | What it does | Phase |
|------|-------------|-------|
| **Survey** | Analyze requirements, produce blueprint | Phase 1 |
| **Found** | Design and cast ingots from blueprint | Phase 2 |
| **Forge** | Execute an ingot: strike, run commands, verify | Phase 3 |
| **Strike** | Send work to the smith (Claude) and get output | Phase 3 |
| **Smelt** | Process raw ore into workable material | Phase 3 |
| **Re-smelt** | Analyze a cracked ingot and rewrite/split it | Phase 3 (recovery) |
| **Reconsider** | Rethink a twice-cracked ingot's fundamental approach | Phase 3 (recovery) |
| **Temper** | Track and display forging progress | Phase 3 |
| **Review** | Master agent CI checks and code review before merge | Phase 3.5 (--worktree) |
| **Assay** | Final quality check, produce report | Phase 4 |
| **Crack** | Fail permanently after exhausting all heats | Terminal state |

### Ingot lifecycle

```
ore --> molten --> forged
                   \--> cracked --> [re-smelt] --> ore (retry)
                                                    \--> cracked --> [reconsider] --> ore (rethought)
                                                                                  --> ore + ore (decomposed)
                                                                                  --> cracked (truly impossible)
```

## How it works

slag runs a 4-phase pipeline (5 phases with `--worktree`):

```
PRD.md --> SURVEYOR --> BLUEPRINT.md --> FOUNDER --> PLAN.md --> FORGE --> PROGRESS.md
 (ore)    (analyze)    (blueprint)     (design)   (crucible)  (strike)    (ledger)

With --worktree (master review enabled):
PRD.md --> SURVEYOR --> FOUNDER --> FORGE (branches) --> REVIEW --> ASSAY
                                         |                  |
                                    git worktrees      CI + AI review
                                    per ingot          before merge
```

### Phase 1: Surveyor

Reads `PRD.md` and produces `BLUEPRINT.md` -- architecture decisions, dependency graph, risk assessment, and forging sequence. Uses Claude's plan mode.

### Phase 2: Founder

Reads the blueprint and casts S-expression ingots into `PLAN.md`:

```
(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5
       :proof "test -f package.json" :work "Initialize project with package.json")
```

### Phase 3: Forge

The main loop. For each ingot:

1. **Pick** the next ore-status ingot
2. **Strike** -- invoke Claude with the task, context, and skill tools
3. **Run** -- extract and execute shell commands from Claude's output
4. **Proof** -- run the `:proof` command; exit 0 = forged, non-zero = retry

Independent ingots (`:solo t`) run on parallel anvils. Sequential ingots (`:solo nil`) run one at a time.

### Phase 3.5: Review (with `--worktree`)

When `--worktree` is enabled, each ingot is forged in an isolated git worktree branch (`forge/iN`). After forging completes, the Review phase:

1. **CI Checks** -- runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --all` on each branch
2. **Master Review** -- AI agent reviews the diff, code quality, and integration safety
3. **Merge Decision** -- approved branches merge to main; rejected branches are flagged

Use `--ci-only` to skip AI review and auto-merge on CI pass. Use `--keep-branches` to preserve branches for debugging.

### Phase 3.6: Analysis & Retry

When ingots crack, slag analyzes failures and can retry automatically (up to `--retry N` cycles):

1. **Failure detection** -- identifies patterns: missing dependencies, protocol failures, proof mismatches, JSON errors
2. **Fix application** -- converts parallel ingots to sequential if they have dependencies
3. **Regeneration** -- uses founder to regenerate ingots that can't be fixed simply
4. **Retry** -- re-runs forge with fixed/regenerated ingots
5. **Force retry prompt** -- when no recoverable ingots found, asks user to confirm force retry

This loop continues until all ingots forge, max retries exhausted, or user declines force retry.

### Phase 4: Assay

Final report. Counts forged vs cracked, writes results to `PROGRESS.md`.

## Ingot fields

```
(ingot :id "i3" :status ore :solo t :grade 2 :skill web :heat 0 :max 5
       :proof "curl -s localhost:3000/health | grep -q ok"
       :work "Add health check endpoint returning JSON {status: ok}")
```

| Field | Values | Meaning |
|-------|--------|---------|
| `:id` | string | Unique identifier |
| `:status` | ore / molten / forged / cracked | Lifecycle state |
| `:solo` | t / nil | Can run in parallel (t) or must be sequential (nil) |
| `:grade` | 1-5 | Complexity; grade >= 3 uses plan mode |
| `:skill` | default / web / ... | Tool configuration for the smith |
| `:heat` | 0-N | Current retry attempt |
| `:max` | 5-8+ | Max retries before cracking |
| `:smelt` | 0-1 | Re-smelt count (0 = never, 1 = re-smelted once) |
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the AI |

## Project files

| File | Role |
|------|------|
| `PRD.md` | Requirements input (ore) |
| `BLUEPRINT.md` | Surveyor analysis |
| `PLAN.md` | Ingot crucible (task list) |
| `PROGRESS.md` | Work history ledger |
| `AGENTS.md` | Agent recipe docs |
| `logs/` | Debug logs (slag heap) |

## Development

```bash
# Rust binary
cargo test --all
cargo clippy -- -D warnings
cargo run -- "Your commission"

# Website (slag.dev)
cd website
npm install
npm run dev       # Dev server at localhost:5173
npm run build     # Production build
npx wrangler pages deploy dist --project-name=slag-dev

# Bash tests
bash tests/test_slag.sh
```

### Repository structure

```
Cargo.toml              # Rust project
src/                    # Rust source (24 files)
slag.sh                 # Bash orchestrator (legacy)
install.sh              # curl | sh installer
website/                # slag.dev (Vite + Cloudflare Pages)
tests/                  # Bash test suite
example/                # Real slag run outputs
.github/workflows/      # CI + release automation
```

## Requirements

- **Rust binary**: Claude CLI (`claude` in PATH)
- **Bash version**: bash 4+, Claude CLI, curl, sed, awk
- **Optional**: Playwright (for `:skill web` ingots)

## License

MIT

## Warning

slag gives Claude autonomous shell access. It will create files, install packages, and run commands without asking. Use in a dedicated directory or container.
