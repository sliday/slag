#!/usr/bin/env bash
# ╔═══════════════════════════════════════════════════════════════════════════╗
# ║  SLAG - Smelt ideas, skim the bugs, forge the product.                   ║
# ║  Usage: bash slag "Your Commission"                                    ║
# ╚═══════════════════════════════════════════════════════════════════════════╝
#
# ┌─────────────────────────────────────────────────────────────────────────┐
# │                           SLAG WORKFLOW                                 │
# ├─────────────────────────────────────────────────────────────────────────┤
# │                                                                         │
# │   PRD.md (Ore)                                                          │
# │       │                                                                 │
# │       ▼                                                                 │
# │   ┌───────────┐    ┌─────────────┐                                      │
# │   │ SURVEYOR  │───▶│ BLUEPRINT.md│  ◉ Plan Mode + Self-Iterate          │
# │   │ (analyze) │    │ (analysis)  │  NO QUESTIONS - expert decisions     │
# │   └───────────┘    └──────┬──────┘                                      │
# │                           │                                             │
# │                           ▼                                             │
# │   ┌───────────┐    ┌─────────────┐                                      │
# │   │ FOUNDER   │───▶│  PLAN.md    │  ◉ Plan Mode + Self-Iterate          │
# │   │ (design)  │    │ (crucible)  │  Creates S-expr ingots               │
# │   └───────────┘    └──────┬──────┘                                      │
# │                           │                                             │
# │                           ▼                                             │
# │   ┌─────────────────────────────────────────┐                           │
# │   │              FORGE LOOP                 │                           │
# │   │                                         │                           │
# │   │  ┌─────────────────────────────────┐    │                           │
# │   │  │  :solo t ingots?                │    │                           │
# │   │  │  YES → PARALLEL ANVILS (max 3)  │    │                           │
# │   │  │  NO  → SEQUENTIAL               │    │                           │
# │   │  └───────────────┬─────────────────┘    │                           │
# │   │                  ▼                      │                           │
# │   │  ┌─────────────────────────────────┐    │                           │
# │   │  │  Select Smith by :skill tag:    │    │                           │
# │   │  │  • web/frontend → +Playwright   │    │                           │
# │   │  │  • default      → base tools    │    │                           │
# │   │  │  • grade ≥ 3    → plan mode     │    │                           │
# │   │  └───────────────┬─────────────────┘    │                           │
# │   │                  ▼                      │                           │
# │   │  ┌─────────────────────────────────┐    │                           │
# │   │  │  Strike → CMD → Proof           │    │                           │
# │   │  │  PASS: :forged, git commit      │    │                           │
# │   │  │  FAIL: :heat++, retry w/ slag   │    │                           │
# │   │  │  MAX:  → RE-SMELT (1 chance)    │    │                           │
# │   │  │    ♻ analyze logs → rewrite/    │    │                           │
# │   │  │      split → back to ore        │    │                           │
# │   │  │    ✗ impossible → :cracked      │    │                           │
# │   │  └─────────────────────────────────┘    │                           │
# │   └─────────────────────────────────────────┘                           │
# │                                                                         │
# ├─────────────────────────────────────────────────────────────────────────┤
# │  FILES:                         INGOT S-EXPRESSION:                     │
# │  ├── PRD.md        (ore)        (ingot :id "i1"                         │
# │  ├── BLUEPRINT.md  (analysis)          :status ore|molten|forged|cracked│
# │  ├── PLAN.md       (crucible)          :solo t|nil                      │
# │  ├── AGENTS.md     (recipes)           :grade 1-5                       │
# │  ├── PROGRESS.md   (ledger)            :skill web|api|default           │
# │  └── logs/         (slag heap)         :heat 0  :max 5                  │
# │                                        :smelt 0 (re-smelt count)       │
# │  SKILLS/PLUGINS:                       :proof "shell cmd"              │
# │                                        :work "description")             │
# │  • web: +Playwright for browser test                                    │
# │  • api: +curl/httpie tools                                              │
# │  • default: base file/shell tools                                       │
# │                                                                         │
# │  PARALLEL ANVILS:                                                       │
# │  • :solo t ingots run concurrently (up to MAX_ANVILS)                   │
# │  • :solo nil ingots run sequentially (have deps)                        │
# │  • Each anvil is a subshell with own smith                              │
# └─────────────────────────────────────────────────────────────────────────┘

if [[ -z "${BASH_VERSION:-}" ]]; then
    echo "Error: Requires bash. Run: bash $0 $*"
    exit 1
fi

set -e
set -o pipefail

# ═══════════════════════════════════════════════════════════════════════════
# CONFIGURATION
# ═══════════════════════════════════════════════════════════════════════════

# Base smith command
SMITH_BASE="${SLAG_SMITH:-claude --dangerously-skip-permissions -p}"

# Smiths by mode
SMITH="$SMITH_BASE"
SMITH_PLAN="$SMITH_BASE --permission-mode plan"

# Skill-specific smiths (plugins/tools)
# Web/frontend: enable Playwright for browser testing
SMITH_WEB="$SMITH_BASE --allowedTools 'Bash Edit Read Write Playwright'"
SMITH_WEB_PLAN="$SMITH_WEB --permission-mode plan"

# Files
BLUEPRINT="BLUEPRINT.md"  # surveyor's deep analysis
CRUCIBLE="PLAN.md"        # ingot mold (s-expressions)
ORE_FILE="PRD.md"         # raw commission
ALLOY_FILE="AGENTS.md"    # learned recipes
LEDGER="PROGRESS.md"      # work history
LOG_DIR="logs"            # slag heap (debug logs)

# Behavior
MAX_ANVILS=3              # max parallel workers
HIGH_GRADE=3              # grade >= this uses plan mode
MAX_ITERATE=3             # max self-iteration for questions

# ═══════════════════════════════════════════════════════════════════════════
# PALETTE (cold ore → hot metal → pure steel)
# ═══════════════════════════════════════════════════════════════════════════

BOLD='\033[1m'
DIM='\033[2m'
GRAY='\033[0;90m'
RED='\033[0;31m'
ORANGE='\033[38;5;208m'
YELLOW='\033[38;5;220m'
WHITE='\033[1;37m'
NC='\033[0m'

COLD="$GRAY"
WARM="$RED"
HOT="$ORANGE"
BRIGHT="$YELLOW"
PURE="$WHITE"

# ═══════════════════════════════════════════════════════════════════════════
# TUI HELPERS
# ═══════════════════════════════════════════════════════════════════════════

SPARK_FRAMES=('ite' '·te' '··e' '···' 'i··' 'it·')
THINK_FRAMES=('◐' '◓' '◑' '◒')
SPARK_PID=""

sparks_start() {
    local msg="$1" mode="$2"
    local frames
    [[ "$mode" == "think" ]] && frames=("${THINK_FRAMES[@]}") || frames=("${SPARK_FRAMES[@]}")
    local len=${#frames[@]}
    (
        local i=0
        while true; do
            printf "\r   ${HOT}%s${NC} %s" "${frames[i++ % len]}" "$msg"
            sleep 0.15
        done
    ) &
    SPARK_PID=$!
    disown 2>/dev/null || true
}

sparks_stop() {
    [[ -n "$SPARK_PID" ]] && { kill "$SPARK_PID" 2>/dev/null || true; wait "$SPARK_PID" 2>/dev/null || true; }
    SPARK_PID=""
    printf "\r\033[K"
}

trap 'sparks_stop; exit' INT TERM

hr() { printf "${GRAY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}\n"; }

header() {
    echo ""
    hr
    printf "${BOLD}${WHITE}  ⚒ %s${NC}\n" "$1"
    hr
}

status_line() { printf "  ${2}%s${NC} %s\n" "$1" "$3"; }

ingot_status() {
    local total forged cracked molten ore
    total=$(grep -c "^(ingot" "$CRUCIBLE" 2>/dev/null) || total=0
    forged=$(grep -c ":status forged" "$CRUCIBLE" 2>/dev/null) || forged=0
    cracked=$(grep -c ":status cracked" "$CRUCIBLE" 2>/dev/null) || cracked=0
    molten=$(grep -c ":status molten" "$CRUCIBLE" 2>/dev/null) || molten=0
    total=${total//[^0-9]/}; total=${total:-0}
    forged=${forged//[^0-9]/}; forged=${forged:-0}
    cracked=${cracked//[^0-9]/}; cracked=${cracked:-0}
    molten=${molten//[^0-9]/}; molten=${molten:-0}
    ore=$((total - forged - cracked - molten))
    printf "${GRAY}[${NC}"
    printf "${PURE}█%d${NC} " "$forged"
    printf "${HOT}▣%d${NC} " "$molten"
    printf "${COLD}░%d${NC}" "$ore"
    [[ $cracked -gt 0 ]] && printf " ${RED}✗%d${NC}" "$cracked"
    printf "${GRAY}]${NC}"
}

temper_bar() {
    local total forged pct filled empty i
    total=$(grep -c "^(ingot" "$CRUCIBLE" 2>/dev/null) || total=0
    total=${total//[^0-9]/}; total=${total:-1}; [[ $total -eq 0 ]] && total=1
    forged=$(grep -c ":status forged" "$CRUCIBLE" 2>/dev/null) || forged=0
    forged=${forged//[^0-9]/}; forged=${forged:-0}
    pct=$((forged * 100 / total)); filled=$((forged * 20 / total)); empty=$((20 - filled))
    printf "  ${GRAY}[${NC}"
    for ((i=0; i<filled; i++)); do
        if ((i < filled / 3)); then printf "${RED}▒${NC}"
        elif ((i < filled * 2 / 3)); then printf "${ORANGE}▓${NC}"
        else printf "${YELLOW}█${NC}"
        fi
    done
    for ((i=0; i<empty; i++)); do printf "${GRAY}░${NC}"; done
    printf "${GRAY}]${NC} ${WHITE}%d%%${NC}\n" "$pct"
}

show_banner() {
    printf "\n"
    printf "  ${GRAY}░░░${NC}${RED}▒${NC}${ORANGE}▒${NC}${YELLOW}▓${NC}${WHITE}█${NC}  ${BOLD}${WHITE}SLAG${NC}  ${WHITE}█${NC}${YELLOW}▓${NC}${ORANGE}▒${NC}${RED}▒${NC}${GRAY}░░░${NC}\n"
    printf "  ${GRAY}cold      hot       pure${NC}\n"
    printf "  ${GRAY}survey · design · forge · temper${NC}\n"
}

# ═══════════════════════════════════════════════════════════════════════════
# UTILITIES
# ═══════════════════════════════════════════════════════════════════════════

mkdir -p "$LOG_DIR"

log() {
    local ts file
    ts=$(date +%Y%m%d_%H%M%S)
    file="$LOG_DIR/${ts}_${1}.log"
    echo "$2" > "$file"
    echo "[$ts] [$1] $file" >> "$LOG_DIR/stream.log"
}

sed_i() {
    if [[ "$OSTYPE" == darwin* ]]; then sed -i '' "$@"; else sed -i "$@"; fi
}

crucible_replace() {
    # Replace an ingot line in PLAN.md by :id with new content (single or multi-line)
    local target_id="$1" replacement="$2"
    local tmp="${CRUCIBLE}.tmp"
    while IFS= read -r line; do
        if [[ "$line" == *":id \"${target_id}\""* ]]; then
            printf '%s\n' "$replacement"
        else
            printf '%s\n' "$line"
        fi
    done < "$CRUCIBLE" > "$tmp" && mv "$tmp" "$CRUCIBLE"
}

sexp_get() {
    echo "$1" | grep -o ":$2 [^ )]*" | head -1 | sed "s/:$2 //" | tr -d '"'
}

sexp_get_quoted() {
    echo "$1" | grep -o ":$2 \"[^\"]*\"" | head -1 | sed "s/:$2 \"//" | sed 's/"$//'
}

truncate_str() {
    [[ ${#1} -gt $2 ]] && echo "${1:0:$2}..." || echo "$1"
}

# Detect if output contains questions (needs self-iteration)
has_questions() {
    echo "$1" | grep -qE '(\?\s*$|^(\*\*)?Question|Which.*\?|What.*\?|Should.*\?|Do you.*\?|Would you.*\?|Can you.*\?|Could you.*\?)'
}

# Self-iterate to resolve questions
self_iterate() {
    local raw="$1" smith="$2" context="$3" iterations=0
    
    while has_questions "$raw" && [[ $iterations -lt $MAX_ITERATE ]]; do
        ((iterations++))
        log "${context}_ITERATE_${iterations}" "$raw"
        sparks_start "resolving ($iterations)..." "think"
        raw=$($smith <<< "$raw

---
[SELF-QUERY RESOLUTION]
You asked questions above. You are the expert. Answer them yourself:
- Make decisive choices based on best practices
- Choose the most sensible option when uncertain
- Do not ask for clarification - decide and proceed

Now output the COMPLETE deliverable with all decisions made.
NO QUESTIONS. NO PREAMBLE. Just the final output.") || { sparks_stop; break; }
        sparks_stop
        log "${context}_RESOLVED_${iterations}" "$raw"
    done
    
    echo "$raw"
}

# Select smith based on skill and grade
select_smith() {
    local skill="$1" grade="$2"
    local smith="$SMITH"
    
    # Skill-based selection
    case "$skill" in
        web|frontend|ui|css|html)
            [[ $grade -ge $HIGH_GRADE ]] && smith="$SMITH_WEB_PLAN" || smith="$SMITH_WEB"
            ;;
        *)
            [[ $grade -ge $HIGH_GRADE ]] && smith="$SMITH_PLAN" || smith="$SMITH"
            ;;
    esac
    
    echo "$smith"
}

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 1: SURVEYOR
# Deep analysis with plan mode. Self-iterates to resolve any questions.
# Output: BLUEPRINT.md
# ═══════════════════════════════════════════════════════════════════════════

run_surveyor() {
    header "SURVEYOR · deep analysis"
    local ore prompt raw
    ore=$(cat "$ORE_FILE")
    
    prompt="ROLE: Master Surveyor. Analyze this commission as domain expert.

COMMISSION:
$ore

Create a thorough BLUEPRINT:

## 1. OVERVIEW
What are we building? 2-3 sentence summary.

## 2. COMPONENTS
List each major piece:
- Name
- Purpose
- Complexity (1-5)
- Dependencies
- Skill: web|api|cli|default

## 3. ARCHITECTURE
\`\`\`
dir/
├── file structure
└── layout
\`\`\`
Key interfaces and data flow.

## 4. DEPENDENCY GRAPH
\`\`\`
[A] ──▶ [B] ──▶ [C]
         │
         └────▶ [D]
\`\`\`

## 5. RISKS
- High complexity areas
- Integration points
- External dependencies

## 6. FORGING SEQUENCE
1. Foundation (parallel, :solo t)
2. Core logic
3. Integration
4. Polish/deploy

## 7. ACCEPTANCE CRITERIA
- Specific tests
- Features to verify
- Quality checks

RULES:
- You are the EXPERT. Make ALL decisions yourself.
- NO QUESTIONS. If uncertain, choose the best option.
- NO PREAMBLE. Output ONLY the blueprint markdown."

    log "SURVEY_PROMPT" "$prompt"
    sparks_start "surveying..." "think"
    raw=$($SMITH_PLAN <<< "$prompt") || { sparks_stop; status_line "✗" "$RED" "Survey failed"; exit 1; }
    sparks_stop
    log "SURVEY_RAW" "$raw"
    
    # Self-iterate if questions detected
    raw=$(self_iterate "$raw" "$SMITH_PLAN" "SURVEY")
    
    echo "$raw" > "$BLUEPRINT"
    status_line "█" "$PURE" "Blueprint: $BLUEPRINT"
    
    echo ""
    local lines
    lines=$(wc -l < "$BLUEPRINT"); lines=${lines//[^0-9]/}
    head -20 "$BLUEPRINT" | while IFS= read -r line; do
        printf "  ${GRAY}%s${NC}\n" "$line"
    done
    [[ $lines -gt 20 ]] && printf "\n  ${GRAY}... +%d lines${NC}\n" $((lines - 20))
}

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 2: FOUNDER
# Creates S-expression ingots with :skill tags for tool selection.
# Uses plan mode. Self-iterates to resolve questions.
# Output: PLAN.md
# ═══════════════════════════════════════════════════════════════════════════

run_founder() {
    header "FOUNDER · casting mold"
    local ore blueprint prompt raw ingots count
    ore=$(cat "$ORE_FILE")
    blueprint=$(cat "$BLUEPRINT" 2>/dev/null || echo "No blueprint")
    
    prompt="ROLE: Master Founder. Cast ingots from blueprint.

COMMISSION:
$ore

BLUEPRINT:
$blueprint

OUTPUT: S-expressions only. One per line. No prose.

TEMPLATE:
(ingot :id \"i1\" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :proof \"SHELL\" :work \"Task\")

FIELDS:
- :id = unique (i1, i2, ...)
- :status = ore (always)
- :solo = t (parallel ok, no deps) | nil (sequential, has deps)
- :grade = 1-5 complexity (3+ gets plan mode)
- :skill = web|api|cli|default (selects tools/plugins)
  - web: browser/frontend tasks, enables Playwright
  - api: HTTP/backend tasks
  - cli: shell/system tasks
  - default: general file operations
- :heat = 0
- :max = attempts (5 simple, 8+ complex)
- :smelt = 0 (re-smelt count; system manages this)
- :proof = shell verification command

EXAMPLES:
(ingot :id \"i1\" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :proof \"test -f package.json\" :work \"Init project\")
(ingot :id \"i2\" :status ore :solo t :grade 2 :skill web :heat 0 :max 5 :proof \"test -f index.html\" :work \"Create HTML structure\")
(ingot :id \"i3\" :status ore :solo nil :grade 2 :skill web :heat 0 :max 5 :proof \"grep -q 'mobile' style.css\" :work \"Mobile-first CSS\")
(ingot :id \"i4\" :status ore :solo nil :grade 3 :skill web :heat 0 :max 8 :proof \"npx playwright test\" :work \"E2E browser tests\")
(ingot :id \"i5\" :status ore :solo nil :grade 2 :skill cli :heat 0 :max 5 :proof \"wrangler pages deploy --dry-run\" :work \"Deploy to Cloudflare\")

PROOF COMMANDS:
- test -f FILE / test -d DIR
- grep -q PATTERN FILE
- node --check FILE
- npm test / npx playwright test
- curl -s URL | grep -q PATTERN

RULES:
- Follow blueprint dependency graph
- :solo t for independent tasks (can parallel)
- :solo nil for dependent tasks (sequential)
- Prefer grade 1-2, split complex work
- Match :skill to task type
- Every :proof must be executable shell

OUTPUT ONLY S-EXPRESSIONS:"

    log "FOUNDER_PROMPT" "$prompt"
    sparks_start "casting..." "think"
    raw=$($SMITH_PLAN <<< "$prompt") || { sparks_stop; status_line "✗" "$RED" "Founder failed"; exit 1; }
    sparks_stop
    log "FOUNDER_RAW" "$raw"
    
    # Self-iterate if questions
    raw=$(self_iterate "$raw" "$SMITH_PLAN" "FOUNDER")
    
    ingots=$(echo "$raw" | grep "^(ingot" || true)
    [[ -z "$ingots" ]] && { status_line "✗" "$RED" "No ingots"; echo "$raw"; exit 1; }
    
    { echo ";; CRUCIBLE $(date)"; echo ";; Blueprint: $BLUEPRINT"; echo "$ingots"; } > "$CRUCIBLE"
    count=$(echo "$ingots" | wc -l | tr -d ' '); count=${count//[^0-9]/}
    
    local simple=0 complex=0 web=0
    while IFS= read -r ing; do
        local g=$(sexp_get "$ing" "grade"); g=${g:-1}
        local s=$(sexp_get "$ing" "skill")
        ((g >= HIGH_GRADE)) && ((complex++)) || ((simple++))
        [[ "$s" == "web" ]] && ((web++))
    done <<< "$ingots"
    
    status_line "█" "$PURE" "Mold: ${WHITE}$count${NC} ingots (${GRAY}$simple simple${NC}, ${YELLOW}$complex complex${NC}, ${ORANGE}$web web${NC})"
    echo ""
    printf "  ${GRAY}%-5s %-3s %-4s %-7s %s${NC}\n" "ID" "GR" "SOLO" "SKILL" "WORK"
    local shown=0
    while IFS= read -r t; do
        [[ $shown -ge 10 ]] && break
        local tid tgr tsolo tskill tdesc gc sc
        tid=$(sexp_get_quoted "$t" "id")
        tgr=$(sexp_get "$t" "grade"); tgr=${tgr:-1}
        tsolo=$(sexp_get "$t" "solo")
        tskill=$(sexp_get "$t" "skill"); tskill=${tskill:-default}
        tdesc=$(sexp_get_quoted "$t" "work")
        [[ "$tsolo" == "t" ]] && tsolo="∥" || tsolo="→"
        gc="$GRAY"; ((tgr == 2)) && gc="$ORANGE"; ((tgr >= 3)) && gc="$YELLOW"; ((tgr >= 4)) && gc="$WHITE"
        sc="$GRAY"; [[ "$tskill" == "web" ]] && sc="$ORANGE"
        printf "  ${ORANGE}%-5s${NC} ${gc}%-3s${NC} %-4s ${sc}%-7s${NC} %s\n" "$tid" "$tgr" "$tsolo" "$tskill" "$(truncate_str "$tdesc" 38)"
        ((shown++))
    done <<< "$ingots"
    [[ $count -gt 10 ]] && printf "  ${GRAY}+%d more${NC}\n" $((count - 10))
}

# ═══════════════════════════════════════════════════════════════════════════
# FLUX PREPARATION
# ═══════════════════════════════════════════════════════════════════════════

prepare_flux() {
    local ingot_sexp="$1" slag="$2"
    local id work proof heat max grade skill
    id=$(sexp_get_quoted "$ingot_sexp" "id")
    work=$(sexp_get_quoted "$ingot_sexp" "work")
    proof=$(sexp_get_quoted "$ingot_sexp" "proof")
    heat=$(sexp_get "$ingot_sexp" "heat")
    max=$(sexp_get "$ingot_sexp" "max")
    grade=$(sexp_get "$ingot_sexp" "grade")
    skill=$(sexp_get "$ingot_sexp" "skill")
    [[ -z "$proof" ]] && proof="true"
    [[ -z "$max" ]] && max=5
    [[ -z "$grade" ]] && grade=1
    [[ -z "$skill" ]] && skill="default"
    
    cat << EOF
=== FORGE ORDER ===
[${id}] ${work}
Grade: ${grade}$(((grade >= HIGH_GRADE)) && echo " ◉ COMPLEX")
Skill: ${skill}$( [[ "$skill" == "web" ]] && echo " (Playwright available)" )
Heat: ${heat}/${max}
Proof: ${proof}

=== BLUEPRINT ===
$(cat "$BLUEPRINT" 2>/dev/null || echo "None")

=== ALLOY RECIPES ===
$(cat "$ALLOY_FILE" 2>/dev/null || echo "None yet")

=== CRUCIBLE STATE ===
$(cat "$CRUCIBLE")

=== RECENT LEDGER ===
$(tail -25 "$LEDGER" 2>/dev/null || echo "Fresh")

=== GIT DIFF ===
$(git diff --stat HEAD~3 2>/dev/null | tail -20 || echo "No history")

EOF
    if [[ -n "$slag" ]]; then
        printf "!!! CRACKED - PREVIOUS ATTEMPT FAILED !!!\n%s\n!!! ANALYZE AND FIX !!!\n" "$slag"
    else
        cat << EOF
=== INSTRUCTIONS ===
1. Forge this ingot completely
2. Create/modify all necessary files
3. Add useful patterns to AGENTS.md
4. End with exactly: CMD: <shell command to verify>

$(((grade >= HIGH_GRADE)) && echo "◉ COMPLEX - think through edge cases")
$([[ "$skill" == "web" ]] && echo "◉ WEB SKILL - Playwright available for browser testing")

RULES:
- NO QUESTIONS. You are the expert.
- NO PROSE. Just code and CMD.
- The CMD must pass for the ingot to be forged.
EOF
    fi
}

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 3: FORGE (STRIKE INGOT)
# Selects smith based on :skill and :grade. Retries with slag feedback.
# ═══════════════════════════════════════════════════════════════════════════

strike_ingot() {
    local ingot_sexp="$1"
    local id work proof max grade skill
    id=$(sexp_get_quoted "$ingot_sexp" "id")
    work=$(sexp_get_quoted "$ingot_sexp" "work")
    proof=$(sexp_get_quoted "$ingot_sexp" "proof")
    max=$(sexp_get "$ingot_sexp" "max")
    grade=$(sexp_get "$ingot_sexp" "grade")
    skill=$(sexp_get "$ingot_sexp" "skill")
    [[ -z "$proof" ]] && proof="true"
    [[ -z "$max" || "$max" == "0" ]] && max=5
    [[ -z "$grade" ]] && grade=1
    [[ -z "$skill" ]] && skill="default"
    
    # Select appropriate smith
    local active_smith smith_label=""
    active_smith=$(select_smith "$skill" "$grade")
    ((grade >= HIGH_GRADE)) && smith_label=" ${YELLOW}◉${NC}"
    [[ "$skill" == "web" ]] && smith_label="${smith_label} ${ORANGE}⚡${NC}"
    
    local slag="" heat=0
    printf "\n  ${HOT}▣${NC} ${WHITE}[%s]${NC} %s${smith_label}\n" "$id" "$(truncate_str "$work" 42)"
    printf "    ${GRAY}gr:%d skill:%s proof:%s${NC}\n" "$grade" "$skill" "$(truncate_str "$proof" 30)"
    
    while [[ $heat -lt $max ]]; do
        ((heat++))
        sed_i "s/:id \"$id\" \(.*\):heat [0-9]*/:id \"$id\" \1:heat $heat/" "$CRUCIBLE"
        
        local hc="$RED"; ((heat > 2)) && hc="$ORANGE"; ((heat > 3)) && hc="$YELLOW"; ((heat > 4)) && hc="$WHITE"
        printf "    ${hc}⚒ %d/%d${NC} " "$heat" "$max"
        
        local flux response cmd
        flux=$(prepare_flux "$ingot_sexp" "$slag")
        log "FLUX_${id}_${heat}" "$flux"
        
        local spark_msg="forging..."
        ((grade >= HIGH_GRADE)) && spark_msg="planning..."
        [[ "$skill" == "web" ]] && spark_msg="web forging..."
        sparks_start "$spark_msg" "$( ((grade >= HIGH_GRADE)) && echo "think" )"
        
        response=$($active_smith <<< "$flux") || { sparks_stop; slag="Smith error: $?"; printf "${RED}✗${NC}\n"; continue; }
        sparks_stop
        log "STRIKE_${id}_${heat}" "$response"
        
        cmd=$(echo "$response" | grep "^CMD:" | tail -1 | sed 's/^CMD: *//')
        if [[ -z "$cmd" ]]; then
            slag="NO CMD: line in response"
            printf "${RED}✗${NC} no CMD\n"
            continue
        fi
        
        printf "${GRAY}%s${NC} " "$(truncate_str "$cmd" 32)"
        
        local output exit_code
        set +e; output=$(eval "$cmd" 2>&1); exit_code=$?; set -e
        log "ASSAY_${id}_${heat}" "exit=$exit_code
$output"
        
        if [[ $exit_code -eq 0 ]]; then
            if [[ -n "$proof" && "$proof" != "$cmd" && "$proof" != "true" ]]; then
                set +e; output=$(eval "$proof" 2>&1); exit_code=$?; set -e
                if [[ $exit_code -ne 0 ]]; then
                    slag="Proof failed [$proof]: $output"
                    printf "${RED}✗${NC} impure\n"
                    continue
                fi
            fi
            printf "${PURE}█${NC}\n"
            git add -A 2>/dev/null || true
            git commit -m "forge($id): $work" --quiet 2>/dev/null || true
            printf "\n## %s [%s] gr:%d skill:%s\n- %s\n- heats:%d\n" "$(date '+%m-%d %H:%M')" "$id" "$grade" "$skill" "$work" "$heat" >> "$LEDGER"
            return 0
        else
            slag="CMD failed (exit $exit_code): $output"
            printf "${RED}✗${NC}\n"
        fi
    done
    return 1
}

# ═══════════════════════════════════════════════════════════════════════════
# RE-SMELT (analyze failure, rewrite or split ingot)
# When an ingot cracks, call Claude to diagnose root cause and either
# rewrite :work/:proof or split into sub-ingots. One re-smelt per ingot.
# ═══════════════════════════════════════════════════════════════════════════

resmelt_ingot() {
    local ingot_sexp="$1"
    local id work proof grade skill smelt
    id=$(sexp_get_quoted "$ingot_sexp" "id")
    work=$(sexp_get_quoted "$ingot_sexp" "work")
    proof=$(sexp_get_quoted "$ingot_sexp" "proof")
    grade=$(sexp_get "$ingot_sexp" "grade")
    skill=$(sexp_get "$ingot_sexp" "skill")
    smelt=$(sexp_get "$ingot_sexp" "smelt")
    [[ -z "$grade" ]] && grade=1
    [[ -z "$skill" ]] && skill="default"
    [[ -z "$smelt" ]] && smelt=0

    # Only one re-smelt allowed
    if [[ "$smelt" -ge 1 ]]; then
        printf "    ${RED}⚠${NC} already re-smelted, truly cracked\n"
        return 1
    fi

    printf "\n  ${ORANGE}♻${NC} ${WHITE}RE-SMELTING [%s]${NC} — analyzing failure...\n" "$id"

    # Gather failure logs
    local failure_logs=""
    for logfile in "$LOG_DIR"/*_${id}_*.log; do
        [[ -f "$logfile" ]] && failure_logs+="--- $(basename "$logfile") ---
$(tail -50 "$logfile")
"
    done

    # Build re-smelt prompt (written to temp file to avoid heredoc quoting issues)
    local prompt_file="${LOG_DIR}/resmelt_prompt_${id}.txt"
    {
        echo "=== RE-SMELT ANALYSIS ==="
        echo "An ingot cracked after exhausting all retry heats. Analyze the failure and fix it."
        echo ""
        echo "CRACKED INGOT:"
        echo "$ingot_sexp"
        echo ""
        echo "BLUEPRINT:"
        cat "$BLUEPRINT" 2>/dev/null || echo "None"
        echo ""
        echo "CRUCIBLE STATE:"
        cat "$CRUCIBLE"
        echo ""
        echo "FAILURE LOGS:"
        echo "$failure_logs"
        echo ""
        echo "GIT STATE:"
        git diff --stat HEAD~5 2>/dev/null | tail -20 || echo "No history"
        git log --oneline -5 2>/dev/null || echo "No commits"
        echo ""
        echo "=== YOUR TASK ==="
        echo "Analyze WHY this ingot failed. Then choose ONE action:"
        echo ""
        echo "OPTION A - REWRITE: If the work or proof was wrong, output a corrected ingot."
        echo "OPTION B - SPLIT: If the task is too big, split into 2-4 smaller sub-ingots."
        echo "OPTION C - IMPOSSIBLE: If this genuinely cannot be done."
        echo ""
        echo "OUTPUT FORMAT (exactly one of):"
        echo ""
        echo "REWRITE:"
        printf '(ingot :id "%s" :status ore :solo t :grade %s :skill %s :heat 0 :max 5 :smelt 1 :proof "CORRECTED_PROOF" :work "Corrected task description")\n' "$id" "$grade" "$skill"
        echo ""
        echo "SPLIT:"
        printf '(ingot :id "%sa" :status ore :solo t :grade G :skill S :heat 0 :max 5 :smelt 1 :proof "PROOF" :work "Sub-task 1")\n' "$id"
        printf '(ingot :id "%sb" :status ore :solo t :grade G :skill S :heat 0 :max 5 :smelt 1 :proof "PROOF" :work "Sub-task 2")\n' "$id"
        echo ""
        echo "IMPOSSIBLE:"
        echo "IMPOSSIBLE: reason"
        echo ""
        echo "RULES:"
        echo "- ALL rewritten/split ingots MUST have :smelt 1"
        echo "- Fix the ROOT CAUSE, do not just retry the same thing"
        echo "- If proof command was wrong, fix the proof"
        echo "- If work was too vague, make it specific"
        echo "- If task was too large, split into focused sub-tasks"
        echo "- Output ONLY the action keyword and ingot lines, nothing else"
    } > "$prompt_file"
    local prompt
    prompt=$(cat "$prompt_file")

    log "RESMELT_${id}" "$prompt"

    sparks_start "re-smelting..."
    local response
    response=$($SMITH <<< "$prompt") || { sparks_stop; printf "    ${RED}✗${NC} smelter failed\n"; return 1; }
    sparks_stop
    log "RESMELT_RESULT_${id}" "$response"

    # Parse response
    if echo "$response" | grep -q "^IMPOSSIBLE:"; then
        local reason
        reason=$(echo "$response" | grep "^IMPOSSIBLE:" | sed 's/^IMPOSSIBLE: *//')
        printf "    ${RED}✗${NC} impossible: %s\n" "$(echo "$reason" | head -c 60)"
        return 1
    elif echo "$response" | grep -q "^REWRITE:"; then
        local new_ingot
        new_ingot=$(echo "$response" | grep "^(ingot" | head -1)
        if [[ -z "$new_ingot" ]]; then
            printf "    ${RED}✗${NC} bad rewrite output\n"
            return 1
        fi
        # Replace cracked ingot with rewritten one
        crucible_replace "$id" "$new_ingot"
        printf "    ${YELLOW}♻${NC} rewritten: %s\n" "$(sexp_get_quoted "$new_ingot" "work" | head -c 50)"
        return 0
    elif echo "$response" | grep -q "^SPLIT:"; then
        local new_ingots
        new_ingots=$(echo "$response" | grep "^(ingot")
        if [[ -z "$new_ingots" ]]; then
            printf "    ${RED}✗${NC} bad split output\n"
            return 1
        fi
        crucible_replace "$id" "$new_ingots"
        local count
        count=$(echo "$new_ingots" | grep -c "^(ingot")
        printf "    ${YELLOW}♻${NC} split into %d sub-ingots\n" "$count"
        return 0
    else
        # Try to find ingot lines anyway (flexible parsing)
        local new_ingots
        new_ingots=$(echo "$response" | grep "^(ingot")
        if [[ -n "$new_ingots" ]]; then
            crucible_replace "$id" "$new_ingots"
            local count
            count=$(echo "$new_ingots" | grep -c "^(ingot")
            printf "    ${YELLOW}♻${NC} re-smelted into %d ingot(s)\n" "$count"
            return 0
        fi
        printf "    ${RED}✗${NC} could not parse smelter output\n"
        return 1
    fi
}

# ═══════════════════════════════════════════════════════════════════════════
# PARALLEL ANVILS
# Runs :solo t ingots concurrently. Each anvil is independent subshell.
# ═══════════════════════════════════════════════════════════════════════════

run_anvils() {
    local pids=() ids=() count=0 ingots
    ingots=$(grep ":status ore" "$CRUCIBLE" | grep ":solo t" || true)
    [[ -z "$ingots" ]] && return 1
    
    while IFS= read -r ingot; do
        [[ -z "$ingot" || $count -ge $MAX_ANVILS ]] && continue
        local id=$(sexp_get_quoted "$ingot" "id")
        [[ "$(sexp_get "$ingot" "solo")" != "t" ]] && continue
        
        # Mark as molten before spawning
        sed_i "s/:id \"$id\" :status ore/:id \"$id\" :status molten/" "$CRUCIBLE"
        
        # Spawn anvil (subshell)
        (
            if strike_ingot "$ingot"; then
                sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status forged/" "$CRUCIBLE"
            elif resmelt_ingot "$ingot"; then
                # Re-smelted: reset to ore for next forge loop iteration
                sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status ore/" "$CRUCIBLE" 2>/dev/null || true
            else
                sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status cracked/" "$CRUCIBLE"
            fi
        ) &
        pids+=($!); ids+=("$id"); ((count++))
    done <<< "$ingots"
    
    if [[ ${#pids[@]} -gt 0 ]]; then
        printf "\n  ${ORANGE}⚒${NC}${YELLOW}⚒${NC}${WHITE}⚒${NC} ${GRAY}%d anvils:${NC} ${WHITE}%s${NC}\n" "${#pids[@]}" "${ids[*]}"
        for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
        return 0
    fi
    return 1
}

# ═══════════════════════════════════════════════════════════════════════════
# FIRE FURNACE (INIT)
# ═══════════════════════════════════════════════════════════════════════════

fire_furnace() {
    header "FIRING FURNACE"
    git init -b main 2>/dev/null || git init 2>/dev/null || true
    [[ ! -f .gitignore ]] || ! grep -q "^logs/$" .gitignore 2>/dev/null && echo "logs/" >> .gitignore
    
    if [[ ! -f "$ORE_FILE" ]]; then
        { echo "# Commission"; echo ""; echo "$1"; } > "$ORE_FILE"
        status_line "░" "$COLD" "Ore loaded"
    fi
    [[ ! -f "$ALLOY_FILE" ]] && { echo "## Alloy Recipes" > "$ALLOY_FILE"; status_line "+" "$GRAY" "Recipes ready"; }
    [[ ! -f "$LEDGER" ]] && { echo "# Smithy Ledger"; echo "Fired: $(date)" > "$LEDGER"; status_line "+" "$GRAY" "Ledger open"; }
    
    git add -A 2>/dev/null || true
    git commit -m "fire: furnace lit" --quiet 2>/dev/null || true
    status_line "█" "$HOT" "Furnace hot"
}

# ═══════════════════════════════════════════════════════════════════════════
# CHECK FORGE (RESUME/RESET)
# ═══════════════════════════════════════════════════════════════════════════

check_forge() {
    [[ ! -f "$ORE_FILE" ]] && return 1
    
    local commission total forged cracked has_bp
    commission=$(tail -1 "$ORE_FILE" | head -c 50)
    total=$(grep -c "^(ingot" "$CRUCIBLE" 2>/dev/null) || total=0
    forged=$(grep -c ":status forged" "$CRUCIBLE" 2>/dev/null) || forged=0
    cracked=$(grep -c ":status cracked" "$CRUCIBLE" 2>/dev/null) || cracked=0
    total=${total//[^0-9]/}; forged=${forged//[^0-9]/}; cracked=${cracked//[^0-9]/}
    [[ -f "$BLUEPRINT" ]] && has_bp="yes" || has_bp="no"
    
    echo ""
    printf "  ${ORANGE}Commission:${NC} %s...\n" "$commission"
    printf "  ${GRAY}Blueprint: %s${NC}\n" "$has_bp"
    [[ $total -gt 0 ]] && { printf "  ${GRAY}Progress: %d/%d${NC}" "$forged" "$total"; [[ $cracked -gt 0 ]] && printf " ${RED}(%d cracked)${NC}" "$cracked"; }
    echo ""
    
    if [[ -n "$1" ]]; then
        printf "\n  ${WHITE}[c]${NC}ontinue  ${WHITE}[r]${NC}emelt  ${WHITE}[q]${NC}uench: "
        read -r choice
        case "$choice" in
            r|R) rm -f "$CRUCIBLE" "$BLUEPRINT"
                 echo "# Smithy Ledger" > "$LEDGER"; echo "Remelt: $(date)" >> "$LEDGER"
                 echo "# Commission" > "$ORE_FILE"; echo "" >> "$ORE_FILE"; echo "$1" >> "$ORE_FILE"
                 printf "  ${ORANGE}Remelting${NC}\n" ;;
            q|Q) printf "  ${GRAY}Quenched${NC}\n"; exit 0 ;;
            *) printf "  ${YELLOW}Continuing${NC}\n" ;;
        esac
    else
        printf "\n  ${WHITE}[c]${NC}ontinue  ${WHITE}[s]${NC}urvey  ${WHITE}[r]${NC}ecast  ${WHITE}[n]${NC}ew  ${WHITE}[q]${NC}uench [c]: "
        read -r choice
        case "$choice" in
            s|S) rm -f "$BLUEPRINT"; printf "  ${ORANGE}Resurveying${NC}\n" ;;
            r|R) rm -f "$CRUCIBLE"; printf "  ${ORANGE}Recasting${NC}\n" ;;
            n|N) printf "  ${GRAY}Commission:${NC} "; read -r nc
                 [[ -n "$nc" ]] && { rm -f "$CRUCIBLE" "$LEDGER" "$BLUEPRINT"
                 echo "# Commission" > "$ORE_FILE"; echo "" >> "$ORE_FILE"; echo "$nc" >> "$ORE_FILE"; } ;;
            q|Q) printf "  ${GRAY}Quenched${NC}\n"; exit 0 ;;
            *) printf "  ${YELLOW}Continuing${NC}\n" ;;
        esac
    fi
    return 0
}

# ═══════════════════════════════════════════════════════════════════════════
# ASSAY (FINAL REPORT)
# ═══════════════════════════════════════════════════════════════════════════

show_assay() {
    local total forged cracked
    total=$(grep -c "^(ingot" "$CRUCIBLE" 2>/dev/null) || total=0
    forged=$(grep -c ":status forged" "$CRUCIBLE" 2>/dev/null) || forged=0
    cracked=$(grep -c ":status cracked" "$CRUCIBLE" 2>/dev/null) || cracked=0
    total=${total//[^0-9]/}; forged=${forged//[^0-9]/}; cracked=${cracked//[^0-9]/}
    
    header "ASSAY"
    printf "  ${WHITE}%d${NC} ingots  ${PURE}%d${NC} forged" "$total" "$forged"
    [[ $cracked -gt 0 ]] && printf "  ${RED}%d${NC} cracked" "$cracked"
    echo ""
    temper_bar
    
    if [[ $cracked -gt 0 ]]; then
        printf "\n  ${RED}Cracked:${NC}\n"
        grep ":status cracked" "$CRUCIBLE" | while IFS= read -r t; do
            printf "    ${RED}✗${NC} [%s] %s\n" "$(sexp_get_quoted "$t" "id")" "$(sexp_get_quoted "$t" "work")"
        done
    fi
    printf "\n  ${GRAY}blueprint: %s${NC}\n" "$BLUEPRINT"
    printf "  ${GRAY}crucible:  %s${NC}\n" "$CRUCIBLE"
    printf "  ${GRAY}slag heap: %s${NC}\n" "$LOG_DIR"
}

# ═══════════════════════════════════════════════════════════════════════════
# MAIN (skipped when sourced for testing)
# ═══════════════════════════════════════════════════════════════════════════

[[ -n "$SLAG_TESTING" ]] && return 0 2>/dev/null || true
[[ -n "$SLAG_TESTING" ]] && exit 0

show_banner

if check_forge "$1"; then
    :
elif [[ -z "$1" ]]; then
    printf "\n  ${RED}Usage:${NC} bash slag.sh \"Commission\"\n\n"
    exit 1
else
    fire_furnace "$1"
fi

# Phase 1: Survey
[[ ! -f "$BLUEPRINT" ]] && run_surveyor

# Phase 2: Found
[[ ! -f "$CRUCIBLE" ]] || ! grep -q "^(ingot" "$CRUCIBLE" && run_founder

# Phase 3: Forge
header "FORGE"
printf "  "; ingot_status; echo ""

while true; do
    if ! grep -q ":status ore\|:status molten" "$CRUCIBLE"; then
        if grep -q ":status cracked" "$CRUCIBLE"; then
            show_assay
            printf "\n  ${RED}${BOLD}✗ CRACKED${NC}\n\n"
            exit 1
        fi
        show_assay
        printf "\n  ${PURE}${BOLD}█ FORGED${NC}\n\n"
        exit 0
    fi
    
    # Parallel anvils for :solo t
    run_anvils && { printf "\n  "; ingot_status; echo ""; continue; }
    
    # Sequential for :solo nil
    ingot=$(grep ":status ore" "$CRUCIBLE" | head -1 || true)
    [[ -z "$ingot" ]] && continue
    id=$(sexp_get_quoted "$ingot" "id")
    sed_i "s/:id \"$id\" :status ore/:id \"$id\" :status molten/" "$CRUCIBLE"
    
    if strike_ingot "$ingot"; then
        sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status forged/" "$CRUCIBLE"
    elif resmelt_ingot "$ingot"; then
        # Re-smelted: reset to ore (or replaced with sub-ingots) — continue forging
        sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status ore/" "$CRUCIBLE" 2>/dev/null || true
    else
        sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status cracked/" "$CRUCIBLE"
        show_assay
        printf "\n  ${RED}${BOLD}✗ CRACKED: %s${NC}\n\n" "$id"
        exit 1
    fi
    printf "\n  "; ingot_status; echo ""
done