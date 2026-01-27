#!/usr/bin/env bash
# slag - smelt the ore, skim the dross, forge the steel
# Usage: bash slag.sh "Your Project Goal"

if [[ -z "${BASH_VERSION:-}" ]]; then
    echo "Error: Requires bash. Run: bash $0 $*"
    exit 1
fi

set -e
set -o pipefail

SMITH="${SLAG_SMITH:-claude --dangerously-skip-permissions}"
CRUCIBLE="PLAN.md"
ORE_FILE="PRD.md"
ALLOY_FILE="AGENTS.md"
LEDGER="PROGRESS.md"
LOG_DIR="logs"
MAX_ANVILS=3

# --- Heated Metal Palette ---
BOLD='\033[1m'
DIM='\033[2m'
BLACK='\033[0;30m'
GRAY='\033[0;90m'
RED='\033[0;31m'
ORANGE='\033[38;5;208m'
YELLOW='\033[38;5;220m'
WHITE='\033[1;37m'
NC='\033[0m'

# Temperature states (cold to hot)
COLD="$GRAY"       # ore, waiting
WARM="$RED"        # heating up
HOT="$ORANGE"      # molten, working
BRIGHT="$YELLOW"   # near complete
PURE="$WHITE"      # forged, success

# --- TUI ---
SPARK_FRAMES=('ite' '·te' '··e' '···' 'i··' 'it·')
SPARK_PID=""

sparks_start() {
    local msg="$1"
    (
        local i=0
        while true; do
            printf "\r   ${HOT}%s${NC} %s" "${SPARK_FRAMES[i++ % 6]}" "$msg"
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

# ore=cold, molten=hot, forged=pure, cracked=failed
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
    printf "${PURE}▰%d${NC} " "$forged"
    printf "${HOT}▣%d${NC} " "$molten"
    printf "${COLD}▱%d${NC}" "$ore"
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
        if ((i < filled / 3)); then printf "${RED}▰${NC}"
        elif ((i < filled * 2 / 3)); then printf "${ORANGE}▰${NC}"
        else printf "${YELLOW}▰${NC}"
        fi
    done
    for ((i=0; i<empty; i++)); do printf "${GRAY}▱${NC}"; done
    printf "${GRAY}]${NC} ${WHITE}%d%%${NC}\n" "$pct"
}

show_banner() {
    printf "\n"
    printf "${GRAY}  ┌─────────────────────────────────────┐${NC}\n"
    printf "${GRAY}  │${NC}  ${GRAY}▱▱▱${NC}${RED}▰${NC}${ORANGE}▰${NC}${YELLOW}▰${NC}${WHITE}▰${NC}  ${BOLD}${WHITE}SLAG${NC}  ${WHITE}▰${NC}${YELLOW}▰${NC}${ORANGE}▰${NC}${RED}▰${NC}${GRAY}▱▱▱${NC}  ${GRAY}│${NC}\n"
    printf "${GRAY}  │${NC}  ${GRAY}coldite ite ite ite          pure${NC}  ${GRAY}│${NC}\n"
    printf "${GRAY}  └─────────────────────────────────────┘${NC}\n"
    printf "    ${GRAY}smelt · skim · forge · temper${NC}\n"
}

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

sexp_get() {
    echo "$1" | grep -o ":$2 [^ )]*" | head -1 | sed "s/:$2 //" | tr -d '"'
}

sexp_get_quoted() {
    echo "$1" | grep -o ":$2 \"[^\"]*\"" | head -1 | sed "s/:$2 \"//" | sed 's/"$//'
}

truncate_str() {
    [[ ${#1} -gt $2 ]] && echo "${1:0:$2}..." || echo "$1"
}

# --- Founder ---
run_founder() {
    header "FOUNDER · designing the mold"
    local ore prompt raw ingots count
    ore=$(cat "$ORE_FILE")
    
    prompt="ROLE: Master Founder. Design casting mold for this commission.
COMMISSION: $ore

OUTPUT: S-expressions only. One ingot per line. No prose.

TEMPLATE:
(ingot :id \"i1\" :status ore :solo t :grade 1 :heat 0 :max 5 :proof \"SHELL_CMD\" :work \"What to forge\")

EXAMPLES:
(ingot :id \"i1\" :status ore :solo t :grade 1 :heat 0 :max 5 :proof \"test -f package.json && grep three package.json\" :work \"Smelt base with three.js\")
(ingot :id \"i2\" :status ore :solo t :grade 1 :heat 0 :max 5 :proof \"test -f index.html\" :work \"Cast HTML crucible\")
(ingot :id \"i3\" :status ore :solo nil :grade 2 :heat 0 :max 5 :proof \"node --check src/main.js\" :work \"Forge main entry\")

PROOF COMMANDS (assay the metal):
- test -f FILE
- test -d DIR
- grep -q PAT FILE
- node --check FILE
- npm test

FIELDS:
- :status = ore | molten | forged | cracked
- :solo t = can forge independently
- :solo nil = requires prior ingots
- :grade 1-5 = complexity (prefer 1-2)
- :heat = current attempt (starts 0)
- :max = max heats before cracked
- :proof = shell command to assay

OUTPUT ONLY S-EXPRESSIONS:"

    log "FOUNDER_PROMPT" "$prompt"
    sparks_start "heating crucible..."
    raw=$($SMITH <<< "$prompt") || { sparks_stop; status_line "✗" "$RED" "Founder failed"; exit 1; }
    sparks_stop
    log "FOUNDER_RAW" "$raw"
    
    ingots=$(echo "$raw" | grep "^(ingot" || true)
    [[ -z "$ingots" ]] && { status_line "✗" "$RED" "No ingots cast"; echo "$raw"; exit 1; }
    
    { echo ";; CRUCIBLE $(date)"; echo "$ingots"; } > "$CRUCIBLE"
    count=$(echo "$ingots" | wc -l | tr -d ' '); count=${count//[^0-9]/}
    status_line "▰" "$PURE" "Mold ready: ${WHITE}$count ingots${NC}"
    echo ""
    printf "  ${GRAY}%-5s %-3s %s${NC}\n" "ID" "GR" "WORK"
    local shown=0
    while IFS= read -r t; do
        [[ $shown -ge 8 ]] && break
        local tid tgr tdesc
        tid=$(sexp_get_quoted "$t" "id"); tgr=$(sexp_get "$t" "grade"); tdesc=$(sexp_get_quoted "$t" "work")
        printf "  ${ORANGE}%-5s${NC} ${GRAY}%-3s${NC} %s\n" "$tid" "$tgr" "$(truncate_str "$tdesc" 48)"
        ((shown++))
    done <<< "$ingots"
    [[ $count -gt 8 ]] && printf "  ${GRAY}+%d more ingots${NC}\n" $((count - 8))
}

# --- Flux ---
prepare_flux() {
    local ingot_sexp="$1" slag="$2"
    local id work proof heat max
    id=$(sexp_get_quoted "$ingot_sexp" "id")
    work=$(sexp_get_quoted "$ingot_sexp" "work")
    proof=$(sexp_get_quoted "$ingot_sexp" "proof")
    heat=$(sexp_get "$ingot_sexp" "heat")
    max=$(sexp_get "$ingot_sexp" "max")
    [[ -z "$proof" ]] && proof="true"
    [[ -z "$max" ]] && max=5
    
    cat << EOF
=== FORGE ORDER ===
[${id}] ${work}
Heat: ${heat}/${max}
Proof: ${proof}

=== ALLOY RECIPES (AGENTS.md) ===
$(cat "$ALLOY_FILE" 2>/dev/null || echo "No recipes yet")

=== CRUCIBLE (PLAN.md) ===
$(cat "$CRUCIBLE")

=== SMITHY LEDGER (last 20) ===
$(tail -20 "$LEDGER" 2>/dev/null || echo "Fresh forge")

=== RECENT WORKINGS ===
$(git diff --stat HEAD~2 2>/dev/null | tail -15 || echo "No history")

EOF
    if [[ -n "$slag" ]]; then
        printf "!!! CRACKED - SLAG FOUND !!!\n%s\n!!! REHEAT AND REFORGE !!!\n" "$slag"
    else
        cat << EOF
=== SMITH INSTRUCTIONS ===
1. Forge the ingot
2. Create/modify files as needed
3. Add useful techniques to ALLOY RECIPES (AGENTS.md)
4. MUST end with: CMD: <shell command to proof the work>

NO EXPLANATION. Forge and CMD only.
EOF
    fi
}

# --- Smith ---
strike_ingot() {
    local ingot_sexp="$1"
    local id work proof max
    id=$(sexp_get_quoted "$ingot_sexp" "id")
    work=$(sexp_get_quoted "$ingot_sexp" "work")
    proof=$(sexp_get_quoted "$ingot_sexp" "proof")
    max=$(sexp_get "$ingot_sexp" "max")
    [[ -z "$proof" ]] && proof="true"
    [[ -z "$max" || "$max" == "0" ]] && max=5
    
    local slag="" heat=0
    printf "\n  ${HOT}▣${NC} ${WHITE}[%s]${NC} %s\n" "$id" "$(truncate_str "$work" 50)"
    printf "    ${GRAY}proof: %s${NC}\n" "$(truncate_str "$proof" 45)"
    
    while [[ $heat -lt $max ]]; do
        ((heat++))
        sed_i "s/:id \"$id\" \(.*\):heat [0-9]*/:id \"$id\" \1:heat $heat/" "$CRUCIBLE"
        
        # Heat indicator: more heats = hotter color
        local heat_color="$RED"
        ((heat > 2)) && heat_color="$ORANGE"
        ((heat > 3)) && heat_color="$YELLOW"
        printf "    ${heat_color}⚒ heat %d/%d${NC} " "$heat" "$max"
        
        local flux response cmd
        flux=$(prepare_flux "$ingot_sexp" "$slag")
        log "FLUX_${id}_${heat}" "$flux"
        
        sparks_start "forging..."
        response=$($SMITH <<< "$flux") || { sparks_stop; slag="Smith error"; printf "${RED}✗${NC}\n"; continue; }
        sparks_stop
        log "STRIKE_${id}_${heat}" "$response"
        
        cmd=$(echo "$response" | grep "^CMD:" | tail -1 | sed 's/^CMD: *//')
        if [[ -z "$cmd" ]]; then
            slag="NO CMD: line. Must end with CMD: <proof_command>"
            printf "${RED}✗${NC} no proof\n"
            continue
        fi
        
        printf "${GRAY}%s${NC} " "$(truncate_str "$cmd" 40)"
        
        local output exit_code
        set +e; output=$(eval "$cmd" 2>&1); exit_code=$?; set -e
        log "ASSAY_${id}_${heat}" "exit=$exit_code
$output"
        
        if [[ $exit_code -eq 0 ]]; then
            if [[ -n "$proof" && "$proof" != "$cmd" && "$proof" != "true" ]]; then
                set +e; output=$(eval "$proof" 2>&1); exit_code=$?; set -e
                if [[ $exit_code -ne 0 ]]; then
                    slag="Proof failed: $proof
$output"
                    printf "${RED}✗${NC} impure\n"
                    continue
                fi
            fi
            printf "${PURE}▰${NC} forged\n"
            git add -A 2>/dev/null || true
            git commit -m "forge($id): $work" --quiet 2>/dev/null || true
            printf "\n## %s [%s]\n- %s\n- heats: %d\n" "$(date '+%m-%d %H:%M')" "$id" "$work" "$heat" >> "$LEDGER"
            return 0
        else
            slag="exit $exit_code: $output"
            printf "${RED}✗${NC} cracked\n"
        fi
    done
    return 1
}

# --- Anvils ---
run_anvils() {
    local pids=() ids=() count=0 ingots
    ingots=$(grep ":status ore" "$CRUCIBLE" | grep ":solo t" || true)
    [[ -z "$ingots" ]] && return 1
    
    while IFS= read -r ingot; do
        [[ -z "$ingot" || $count -ge $MAX_ANVILS ]] && continue
        local id=$(sexp_get_quoted "$ingot" "id")
        [[ "$(sexp_get "$ingot" "solo")" != "t" ]] && continue
        sed_i "s/:id \"$id\" :status ore/:id \"$id\" :status molten/" "$CRUCIBLE"
        (
            if strike_ingot "$ingot"; then
                sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status forged/" "$CRUCIBLE"
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

# --- Fire Furnace ---
fire_furnace() {
    header "FIRING FURNACE"
    git init -b main 2>/dev/null || git init 2>/dev/null || true
    [[ ! -f .gitignore ]] || ! grep -q "^logs/$" .gitignore 2>/dev/null && echo "logs/" >> .gitignore
    
    if [[ ! -f "$ORE_FILE" ]]; then
        { echo "# Commission"; echo ""; echo "$1"; } > "$ORE_FILE"
        status_line "▱" "$COLD" "Ore loaded"
    fi
    [[ ! -f "$ALLOY_FILE" ]] && { echo "## Alloy Recipes" > "$ALLOY_FILE"; status_line "+" "$GRAY" "Recipe book ready"; }
    [[ ! -f "$LEDGER" ]] && { echo "# Smithy Ledger"; echo "Furnace fired: $(date)" > "$LEDGER"; status_line "+" "$GRAY" "Ledger opened"; }
    
    git add -A 2>/dev/null || true
    git commit -m "fire: furnace lit" --quiet 2>/dev/null || true
    status_line "▰" "$HOT" "Furnace hot"
}

# --- Check Forge ---
check_forge() {
    [[ ! -f "$ORE_FILE" ]] && return 1
    
    local commission total forged cracked
    commission=$(tail -1 "$ORE_FILE" | head -c 50)
    total=$(grep -c "^(ingot" "$CRUCIBLE" 2>/dev/null) || total=0
    forged=$(grep -c ":status forged" "$CRUCIBLE" 2>/dev/null) || forged=0
    cracked=$(grep -c ":status cracked" "$CRUCIBLE" 2>/dev/null) || cracked=0
    total=${total//[^0-9]/}; forged=${forged//[^0-9]/}; cracked=${cracked//[^0-9]/}
    
    echo ""
    printf "  ${ORANGE}Commission:${NC} %s...\n" "$commission"
    [[ $total -gt 0 ]] && printf "  ${GRAY}Progress: %d/%d forged${NC}" "$forged" "$total" && [[ $cracked -gt 0 ]] && printf " ${RED}(%d cracked)${NC}" "$cracked"
    echo ""
    
    if [[ -n "$1" ]]; then
        printf "\n  ${WHITE}[c]${NC}ontinue  ${WHITE}[r]${NC}emelt  ${WHITE}[q]${NC}uench: "
        read -r choice
        case "$choice" in
            r|R) rm -f "$CRUCIBLE"; echo "# Smithy Ledger" > "$LEDGER"; echo "Remelt: $(date)" >> "$LEDGER"
                 echo "# Commission" > "$ORE_FILE"; echo "" >> "$ORE_FILE"; echo "$1" >> "$ORE_FILE"
                 printf "  ${ORANGE}Remelting${NC}\n" ;;
            q|Q) printf "  ${GRAY}Quenched${NC}\n"; exit 0 ;;
            *) printf "  ${YELLOW}Continuing${NC}\n" ;;
        esac
    else
        printf "\n  ${WHITE}[c]${NC}ontinue  ${WHITE}[r]${NC}ecast  ${WHITE}[n]${NC}ew  ${WHITE}[q]${NC}uench [c]: "
        read -r choice
        case "$choice" in
            r|R) rm -f "$CRUCIBLE"; printf "  ${ORANGE}Recasting${NC}\n" ;;
            n|N) printf "  ${GRAY}Commission:${NC} "; read -r nc
                 [[ -n "$nc" ]] && { rm -f "$CRUCIBLE" "$LEDGER"
                 echo "# Commission" > "$ORE_FILE"; echo "" >> "$ORE_FILE"; echo "$nc" >> "$ORE_FILE"; } ;;
            q|Q) printf "  ${GRAY}Quenched${NC}\n"; exit 0 ;;
            *) printf "  ${YELLOW}Continuing${NC}\n" ;;
        esac
    fi
    return 0
}

# --- Assay ---
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
    printf "\n  ${GRAY}slag heap: %s${NC}\n" "$LOG_DIR"
}

# --- Main ---
show_banner

if check_forge "$1"; then
    :
elif [[ -z "$1" ]]; then
    printf "\n  ${RED}Usage:${NC} bash slag.sh \"Commission\"\n\n"
    exit 1
else
    fire_furnace "$1"
fi

[[ ! -f "$CRUCIBLE" ]] || ! grep -q "^(ingot" "$CRUCIBLE" && run_founder

header "FORGE"
printf "  "; ingot_status; echo ""

while true; do
    if ! grep -q ":status ore\|:status molten" "$CRUCIBLE"; then
        if grep -q ":status cracked" "$CRUCIBLE"; then
            show_assay; printf "\n  ${RED}${BOLD}✗ CRACKED${NC}\n\n"; exit 1
        fi
        show_assay; printf "\n  ${PURE}${BOLD}▰ FORGED${NC}\n\n"; exit 0
    fi
    
    run_anvils && { printf "\n  "; ingot_status; echo ""; continue; }
    
    ingot=$(grep ":status ore" "$CRUCIBLE" | head -1 || true)
    [[ -z "$ingot" ]] && continue
    id=$(sexp_get_quoted "$ingot" "id")
    sed_i "s/:id \"$id\" :status ore/:id \"$id\" :status molten/" "$CRUCIBLE"
    
    if strike_ingot "$ingot"; then
        sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status forged/" "$CRUCIBLE"
    else
        sed_i "s/:id \"$id\" :status molten/:id \"$id\" :status cracked/" "$CRUCIBLE"
        show_assay; printf "\n  ${RED}${BOLD}✗ CRACKED: %s${NC}\n\n" "$id"; exit 1
    fi
    printf "\n  "; ingot_status; echo ""
done