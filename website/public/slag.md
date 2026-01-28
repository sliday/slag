# slag

> Smelt ideas, skim the bugs, forge the product.

Task orchestrator for AI-powered development. Breaks requirements into S-expression ingots and forges them via Claude agents with automatic retry, re-smelt recovery, and proof-based verification.

## Install

```bash
# one-liner (auto-adds to PATH)
curl -sSf https://slag.dev/install.sh | sh

# bash version (no build required)
curl -fsSL https://slag.dev/slag.sh -o /usr/local/bin/slag && chmod +x /usr/local/bin/slag

# from source
cargo install --git https://github.com/sliday/slag
```

## Quick Start

```bash
# write your requirements
cat > PRD.md << 'EOF'
Build a REST API with auth and rate limiting
EOF

# forge
slag "Build the REST API from PRD.md"
```

> **Warning:** slag gives Claude autonomous shell access. Use in a dedicated directory or container.

## Pipeline

```
  PRD.md                                          PROGRESS.md
  (ore)                                             (ledger)
    |                                                 ^
    v                                                 |
 +-----------+    +--------------+    +--------+    +-------+
 | SURVEYOR  |--->| FOUNDER      |--->| FORGE  |--->| ASSAY |
 | analyze   |    | cast ingots  |    | strike |    | report|
 +-----------+    +--------------+    +--------+    +-------+
    |                    |                    |
    v                    v                    v
 BLUEPRINT.md        PLAN.md             git commits
 (analysis)        (s-expr ingots)      (per ingot)
```

## Forge Loop

```
 PICK ORE                       PARALLEL ANVILS
    |                           +---------+---------+
    v                           |         |         |
 :solo t? ----yes-----> ANVIL 1   ANVIL 2   ANVIL 3
    |                      |         |         |
    no                     v         v         v
    |                   (each anvil is independent subshell)
    v
 SELECT SMITH by :skill + :grade
    |
    |  web/frontend --> +Playwright
    |  grade >= 3   --> plan mode
    |  default      --> base tools
    v
 STRIKE (claude invocation)
    |
    v
 CMD (extract & run shell command)
    |
    v
 PROOF (run :proof shell command)
    |
    +----- pass ----> :forged  + git commit
    |
    +----- fail ----> :heat++ retry with slag feedback
    |
    +----- max -----> RE-SMELT (analyze failure)
                         |
                         +----- rewrite --> new ore (retry)
                         +----- split ----> sub-ingots (2-4)
                         +----- impossible -> :cracked
```

## Four Phases

### 1. SURVEYOR
Deep analysis with plan mode. Reads PRD.md (ore), produces BLUEPRINT.md with architecture, dependency graph, risk assessment, and forging sequence. Self-iterates to resolve any ambiguity.

### 2. FOUNDER
Casts S-expression ingots from the blueprint. Each ingot has an ID, complexity grade, skill tag, proof command, and work description. Outputs PLAN.md as the crucible.

### 3. FORGE
Strikes each ingot via Claude. Solo ingots run on parallel anvils (up to 3). Selects smith by skill and grade. Retries with slag feedback on failure. Commits on success.

### 4. ASSAY
Final quality report. Shows forged/cracked counts, temperature bar, and identifies any cracked ingots. Exits 0 on full forge, 1 if any ingot cracked.

## Design Decisions

### Why S-Expressions?
S-expressions are single-line, grep/sed parseable, require zero dependencies, and survive bash string handling. Every ingot is one line in PLAN.md. The entire orchestrator can manipulate state with sed_i without any JSON/YAML parser. Fields are keyword-prefixed (`:id`, `:status`) making them unambiguous to extract with pattern matching.

### Why Parallel Anvils?
Independent ingots (`:solo t`) run concurrently in background subshells, up to MAX_ANVILS=3. This gives 3x throughput for foundation tasks that have no dependencies. Each anvil gets its own smith process. Sequential ingots (`:solo nil`) run one at a time after parallel work completes.

### Why Proof-Based Verification?
Every ingot carries a `:proof` field containing a shell command. Exit code 0 means pass, anything else means fail. No human review needed. Proofs are concrete: `test -f file`, `npm test`, `grep -q pattern file`. This enables fully autonomous forging with machine-verifiable quality gates.

### Why Self-Iteration?
When a surveyor or founder output contains questions, slag detects them and feeds the output back with instructions to resolve autonomously. Up to 3 rounds. This prevents the forge from stalling on ambiguity. The AI is instructed to make expert decisions rather than ask for clarification.

### Why Re-Smelt?
When an ingot cracks after exhausting all heats, re-smelting analyzes the failure logs, blueprint, and git history to diagnose the root cause. It can rewrite the ingot with corrected work/proof, split it into 2-4 focused sub-ingots, or declare it impossible. Each ingot gets one re-smelt attempt — learning from failure instead of brute-forcing retries.

### Why Metallurgical Metaphor?
Unambiguous vocabulary that maps naturally to the pipeline. Ore (raw input) is surveyed, cast into ingots, heated in a forge, and either becomes forged steel or cracked waste. Every term has exactly one meaning. The temperature gradient (cold → hot → pure) maps to progress from unstarted to complete.

## FAQ

### "command not found: slag" after install
The installer adds `~/.slag/bin` to your shell profile automatically. Open a new terminal or run:
```bash
source ~/.zshrc  # or ~/.bashrc
```
If that doesn't work, manually add to your profile: `export PATH="$HOME/.slag/bin:$PATH"`

### How do I update slag?
Run `slag update` to self-update to the latest release. This downloads the new binary from GitHub and replaces the current one.

### What's the difference between binary and bash versions?
The Rust binary is faster, has better error handling, and includes self-update. The bash script is a single file with no build step — useful if you can't install Rust or want to inspect/modify the orchestrator directly.

### Do I need Claude CLI installed?
Yes. slag invokes `claude` via CLI. Install it from [docs.anthropic.com](https://docs.anthropic.com/en/docs/claude-code) and ensure it's in your PATH.

## Ingot S-Expression Format

```lisp
(ingot :id "i1" :status ore :solo t :grade 2 :skill web :heat 0 :max 5
       :proof "test -f index.html && npm test"
       :work "Create project structure with index.html and test suite")
```

## Field Reference

| Field | Values | Meaning |
|-------|--------|---------|
| `:id` | "i1", "i2", ... | Unique ingot identifier |
| `:status` | ore \| molten \| forged \| cracked | Lifecycle state |
| `:solo` | t \| nil | Can run in parallel (t) or must be sequential (nil) |
| `:grade` | 1-5 | Complexity level; grade >= 3 uses plan mode |
| `:skill` | web \| api \| cli \| default | Selects smith tools/plugins |
| `:heat` | 0-N | Current retry attempt |
| `:max` | 5-8+ | Max retries before cracking |
| `:proof` | shell command | Acceptance test (exit 0 = pass) |
| `:work` | string | Task description for the smith |

## Links

- [GitHub](https://github.com/sliday/slag)
- [Releases](https://github.com/sliday/slag/releases)
- [Download bash version](https://slag.dev/slag.sh)

MIT License
