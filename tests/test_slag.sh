#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════
# SLAG TEST SUITE
# Mock tests for slag.sh utility functions and state transitions
# Run: bash tests/test_slag.sh
# ═══════════════════════════════════════════════════════════════════════════

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SLAG_DIR="$(dirname "$SCRIPT_DIR")"
TEST_TMP=""
PASS=0
FAIL=0
TOTAL=0

# ── Test framework ────────────────────────────────────────────────────────

setup() {
    TEST_TMP=$(mktemp -d)
    export LOG_DIR="$TEST_TMP/logs"
    export CRUCIBLE="$TEST_TMP/PLAN.md"
    export BLUEPRINT="$TEST_TMP/BLUEPRINT.md"
    export ORE_FILE="$TEST_TMP/PRD.md"
    export ALLOY_FILE="$TEST_TMP/AGENTS.md"
    export LEDGER="$TEST_TMP/PROGRESS.md"
    mkdir -p "$LOG_DIR"
}

teardown() {
    [[ -d "$TEST_TMP" ]] && rm -rf "$TEST_TMP"
}

assert_eq() {
    local label="$1" expected="$2" actual="$3"
    ((TOTAL++))
    if [[ "$expected" == "$actual" ]]; then
        ((PASS++))
        printf "  \033[32m✓\033[0m %s\n" "$label"
    else
        ((FAIL++))
        printf "  \033[31m✗\033[0m %s\n" "$label"
        printf "    expected: %s\n" "$expected"
        printf "    actual:   %s\n" "$actual"
    fi
}

assert_contains() {
    local label="$1" needle="$2" haystack="$3"
    ((TOTAL++))
    if [[ "$haystack" == *"$needle"* ]]; then
        ((PASS++))
        printf "  \033[32m✓\033[0m %s\n" "$label"
    else
        ((FAIL++))
        printf "  \033[31m✗\033[0m %s\n" "$label"
        printf "    expected to contain: %s\n" "$needle"
        printf "    actual: %s\n" "$haystack"
    fi
}

assert_file_contains() {
    local label="$1" needle="$2" file="$3"
    ((TOTAL++))
    if grep -q "$needle" "$file" 2>/dev/null; then
        ((PASS++))
        printf "  \033[32m✓\033[0m %s\n" "$label"
    else
        ((FAIL++))
        printf "  \033[31m✗\033[0m %s\n" "$label"
        printf "    file %s does not contain: %s\n" "$file" "$needle"
    fi
}

assert_file_not_contains() {
    local label="$1" needle="$2" file="$3"
    ((TOTAL++))
    if ! grep -q "$needle" "$file" 2>/dev/null; then
        ((PASS++))
        printf "  \033[32m✓\033[0m %s\n" "$label"
    else
        ((FAIL++))
        printf "  \033[31m✗\033[0m %s\n" "$label"
        printf "    file %s should not contain: %s\n" "$file" "$needle"
    fi
}

section() {
    printf "\n\033[1m%s\033[0m\n" "$1"
}

# ── Source slag functions ─────────────────────────────────────────────────

export SLAG_TESTING=1
cd "$TEST_TMP" 2>/dev/null || true
source "$SLAG_DIR/slag.sh"

# Override spinner functions for testing (they spawn background processes)
sparks_start() { :; }
sparks_stop() { :; }

# ── Tests: sexp_get ───────────────────────────────────────────────────────

section "sexp_get — extract unquoted fields"

setup
INGOT='(ingot :id "i1" :status ore :solo t :grade 3 :skill web :heat 0 :max 5 :smelt 0 :proof "test -f out.js" :work "Build module")'

assert_eq "extracts :status" "ore" "$(sexp_get "$INGOT" "status")"
assert_eq "extracts :solo" "t" "$(sexp_get "$INGOT" "solo")"
assert_eq "extracts :grade" "3" "$(sexp_get "$INGOT" "grade")"
assert_eq "extracts :heat" "0" "$(sexp_get "$INGOT" "heat")"
assert_eq "extracts :max" "5" "$(sexp_get "$INGOT" "max")"
assert_eq "extracts :smelt" "0" "$(sexp_get "$INGOT" "smelt")"
assert_eq "extracts :skill" "web" "$(sexp_get "$INGOT" "skill")"
teardown

# ── Tests: sexp_get_quoted ────────────────────────────────────────────────

section "sexp_get_quoted — extract quoted fields"

setup
INGOT='(ingot :id "i1" :status ore :solo t :grade 3 :skill web :heat 0 :max 5 :proof "test -f out.js" :work "Build the module")'

assert_eq "extracts :id" "i1" "$(sexp_get_quoted "$INGOT" "id")"
assert_eq "extracts :proof" "test -f out.js" "$(sexp_get_quoted "$INGOT" "proof")"
assert_eq "extracts :work" "Build the module" "$(sexp_get_quoted "$INGOT" "work")"
teardown

# ── Tests: sexp_get edge cases ────────────────────────────────────────────

section "sexp_get — edge cases"

setup
INGOT_NIL='(ingot :id "i2" :status ore :solo nil :grade 1 :skill default :heat 0 :max 8 :proof "true" :work "Simple task")'
INGOT_FORGED='(ingot :id "i3" :status forged :solo t :grade 2 :skill api :heat 4 :max 5 :proof "curl -s localhost" :work "API endpoint")'

assert_eq "solo nil" "nil" "$(sexp_get "$INGOT_NIL" "solo")"
assert_eq "status forged" "forged" "$(sexp_get "$INGOT_FORGED" "status")"
assert_eq "heat after retries" "4" "$(sexp_get "$INGOT_FORGED" "heat")"
assert_eq "missing field returns empty" "" "$(sexp_get "$INGOT_NIL" "nonexistent")"
teardown

# ── Tests: truncate_str ───────────────────────────────────────────────────

section "truncate_str — string truncation"

setup
assert_eq "short string unchanged" "hello" "$(truncate_str "hello" 10)"
assert_eq "exact length unchanged" "hello" "$(truncate_str "hello" 5)"
assert_eq "long string truncated" "hello ..." "$(truncate_str "hello world" 6)"
assert_eq "single char limit" "h..." "$(truncate_str "hello" 1)"
teardown

# ── Tests: crucible_replace ───────────────────────────────────────────────

section "crucible_replace — single ingot replacement"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "First task")
(ingot :id "i2" :status ore :solo nil :grade 2 :skill web :heat 0 :max 5 :proof "test -f x" :work "Second task")
(ingot :id "i3" :status forged :solo t :grade 1 :skill default :heat 1 :max 5 :proof "true" :work "Third task")
EOF

crucible_replace "i2" '(ingot :id "i2" :status ore :solo nil :grade 2 :skill web :heat 0 :max 5 :smelt 1 :proof "test -d build" :work "Revised second task")'

assert_file_contains "replaced ingot present" "Revised second task" "$CRUCIBLE"
assert_file_not_contains "old ingot removed" "Second task" "$CRUCIBLE"
assert_file_contains "i1 untouched" "First task" "$CRUCIBLE"
assert_file_contains "i3 untouched" "Third task" "$CRUCIBLE"
teardown

# ── Tests: crucible_replace — split (multi-line) ─────────────────────────

section "crucible_replace — split into sub-ingots"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i1" :status forged :solo t :grade 1 :skill default :heat 1 :max 5 :proof "true" :work "Done")
(ingot :id "i2" :status ore :solo nil :grade 3 :skill web :heat 0 :max 8 :proof "npm test" :work "Big task")
(ingot :id "i3" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "Pending")
EOF

SPLIT_INGOTS='(ingot :id "i2a" :status ore :solo t :grade 2 :skill web :heat 0 :max 5 :smelt 1 :proof "test -f a.js" :work "Sub-task A")
(ingot :id "i2b" :status ore :solo nil :grade 2 :skill web :heat 0 :max 5 :smelt 1 :proof "test -f b.js" :work "Sub-task B")'

crucible_replace "i2" "$SPLIT_INGOTS"

assert_file_contains "sub-ingot A present" "Sub-task A" "$CRUCIBLE"
assert_file_contains "sub-ingot B present" "Sub-task B" "$CRUCIBLE"
assert_file_not_contains "original removed" "Big task" "$CRUCIBLE"
assert_file_contains "i1 untouched" "Done" "$CRUCIBLE"
assert_file_contains "i3 untouched" "Pending" "$CRUCIBLE"

# Verify order: i1, then i2a/i2b, then i3
line_i1=$(grep -n '"i1"' "$CRUCIBLE" | head -1 | cut -d: -f1)
line_i2a=$(grep -n "i2a" "$CRUCIBLE" | head -1 | cut -d: -f1)
line_i2b=$(grep -n "i2b" "$CRUCIBLE" | head -1 | cut -d: -f1)
line_i3=$(grep -n '"i3"' "$CRUCIBLE" | head -1 | cut -d: -f1)

((TOTAL++))
if [[ $line_i1 -lt $line_i2a && $line_i2a -lt $line_i2b && $line_i2b -lt $line_i3 ]]; then
    ((PASS++)); printf "  \033[32m✓\033[0m %s\n" "sub-ingots inserted in correct position"
else
    ((FAIL++)); printf "  \033[31m✗\033[0m %s (order: i1=%s i2a=%s i2b=%s i3=%s)\n" "sub-ingots inserted in correct position" "$line_i1" "$line_i2a" "$line_i2b" "$line_i3"
fi
teardown

# ── Tests: resmelt_ingot — already smelted ────────────────────────────────

section "resmelt_ingot — guards"

setup
INGOT_SMELTED='(ingot :id "i5" :status cracked :solo t :grade 2 :skill default :heat 5 :max 5 :smelt 1 :proof "test -f x" :work "Already tried")'

# Should return 1 (failure) because smelt >= 1
result=0
resmelt_ingot "$INGOT_SMELTED" > /dev/null 2>&1 || result=$?
assert_eq "rejects already-smelted ingot" "1" "$result"
teardown

# ── Tests: resmelt_ingot — mock REWRITE response ─────────────────────────

section "resmelt_ingot — mock REWRITE"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i1" :status forged :solo t :grade 1 :skill default :heat 1 :max 5 :proof "true" :work "Done")
(ingot :id "i6" :status cracked :solo t :grade 2 :skill web :heat 5 :max 5 :smelt 0 :proof "test -f bad.js" :work "Broken task")
EOF
echo "Survey data" > "$BLUEPRINT"

INGOT_TO_RESMELT='(ingot :id "i6" :status cracked :solo t :grade 2 :skill web :heat 5 :max 5 :smelt 0 :proof "test -f bad.js" :work "Broken task")'

# Mock smith: script that drains stdin and returns a REWRITE response
mock_bin="$TEST_TMP/mock_smith"
cat > "$mock_bin" << 'MOCKSCRIPT'
#!/usr/bin/env bash
cat > /dev/null
echo "REWRITE:"
echo '(ingot :id "i6" :status ore :solo t :grade 2 :skill web :heat 0 :max 5 :smelt 1 :proof "test -f good.js" :work "Fixed task")'
MOCKSCRIPT
chmod +x "$mock_bin"
SMITH="$mock_bin"

result=0
resmelt_ingot "$INGOT_TO_RESMELT" > /dev/null 2>&1 || result=$?
assert_eq "resmelt returns success" "0" "$result"
assert_file_contains "rewritten ingot in crucible" "Fixed task" "$CRUCIBLE"
assert_file_contains "smelt set to 1" ":smelt 1" "$CRUCIBLE"
assert_file_not_contains "old ingot removed" "Broken task" "$CRUCIBLE"
assert_file_contains "other ingots untouched" "Done" "$CRUCIBLE"
teardown

# ── Tests: resmelt_ingot — mock SPLIT response ───────────────────────────

section "resmelt_ingot — mock SPLIT"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i7" :status cracked :solo nil :grade 3 :skill web :heat 5 :max 5 :smelt 0 :proof "npm test" :work "Too big task")
EOF
echo "Survey data" > "$BLUEPRINT"

INGOT_TO_SPLIT='(ingot :id "i7" :status cracked :solo nil :grade 3 :skill web :heat 5 :max 5 :smelt 0 :proof "npm test" :work "Too big task")'

mock_bin="$TEST_TMP/mock_smith"
cat > "$mock_bin" << 'MOCKSCRIPT'
#!/usr/bin/env bash
cat > /dev/null
echo "SPLIT:"
echo '(ingot :id "i7a" :status ore :solo t :grade 2 :skill web :heat 0 :max 5 :smelt 1 :proof "test -f part1.js" :work "Part one")'
echo '(ingot :id "i7b" :status ore :solo nil :grade 2 :skill web :heat 0 :max 5 :smelt 1 :proof "test -f part2.js" :work "Part two")'
MOCKSCRIPT
chmod +x "$mock_bin"
SMITH="$mock_bin"

result=0
resmelt_ingot "$INGOT_TO_SPLIT" > /dev/null 2>&1 || result=$?
assert_eq "split returns success" "0" "$result"
assert_file_contains "sub-ingot A present" "Part one" "$CRUCIBLE"
assert_file_contains "sub-ingot B present" "Part two" "$CRUCIBLE"
assert_file_not_contains "original removed" "Too big task" "$CRUCIBLE"
teardown

# ── Tests: resmelt_ingot — mock IMPOSSIBLE response ──────────────────────

section "resmelt_ingot — mock IMPOSSIBLE"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i8" :status cracked :solo t :grade 1 :skill default :heat 5 :max 5 :smelt 0 :proof "false" :work "Impossible task")
EOF
echo "Survey data" > "$BLUEPRINT"

INGOT_IMPOSSIBLE='(ingot :id "i8" :status cracked :solo t :grade 1 :skill default :heat 5 :max 5 :smelt 0 :proof "false" :work "Impossible task")'

mock_bin="$TEST_TMP/mock_smith"
cat > "$mock_bin" << 'MOCKSCRIPT'
#!/usr/bin/env bash
cat > /dev/null
echo "IMPOSSIBLE: requires external API key that does not exist"
MOCKSCRIPT
chmod +x "$mock_bin"
SMITH="$mock_bin"

result=0
resmelt_ingot "$INGOT_IMPOSSIBLE" > /dev/null 2>&1 || result=$?
assert_eq "impossible returns failure" "1" "$result"
assert_file_contains "original still in crucible" "Impossible task" "$CRUCIBLE"
teardown

# ── Tests: sed_i ──────────────────────────────────────────────────────────

section "sed_i — cross-platform sed in-place"

setup
echo "hello world" > "$TEST_TMP/sedtest.txt"
sed_i "s/world/slag/" "$TEST_TMP/sedtest.txt"
assert_eq "sed_i replaces in-place" "hello slag" "$(cat "$TEST_TMP/sedtest.txt")"
teardown

# ── Tests: ingot state transitions ───────────────────────────────────────

section "state transitions — ore to forged via sed_i"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i10" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "Transition test")
EOF

# ore → molten
sed_i 's/:id "i10" :status ore/:id "i10" :status molten/' "$CRUCIBLE"
assert_file_contains "ore to molten" ":status molten" "$CRUCIBLE"

# molten → forged
sed_i 's/:id "i10" :status molten/:id "i10" :status forged/' "$CRUCIBLE"
assert_file_contains "molten to forged" ":status forged" "$CRUCIBLE"
teardown

section "state transitions — ore to cracked"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i11" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "Crack test")
EOF

sed_i 's/:id "i11" :status ore/:id "i11" :status molten/' "$CRUCIBLE"
sed_i 's/:id "i11" :status molten/:id "i11" :status cracked/' "$CRUCIBLE"
assert_file_contains "molten to cracked" ":status cracked" "$CRUCIBLE"
teardown

# ── Tests: heat increment ────────────────────────────────────────────────

section "heat tracking — increment via sed_i"

setup
cat > "$CRUCIBLE" << 'EOF'
(ingot :id "i12" :status molten :solo t :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "Heat test")
EOF

sed_i 's/:id "i12" \(.*\):heat [0-9]*/:id "i12" \1:heat 1/' "$CRUCIBLE"
assert_file_contains "heat incremented to 1" ":heat 1" "$CRUCIBLE"

sed_i 's/:id "i12" \(.*\):heat [0-9]*/:id "i12" \1:heat 2/' "$CRUCIBLE"
assert_file_contains "heat incremented to 2" ":heat 2" "$CRUCIBLE"
teardown

# ── Results ───────────────────────────────────────────────────────────────

echo ""
printf "═══════════════════════════════════════════\n"
if [[ $FAIL -eq 0 ]]; then
    printf "\033[32m  ✓ ALL %d TESTS PASSED\033[0m\n" "$TOTAL"
else
    printf "\033[31m  ✗ %d/%d FAILED\033[0m\n" "$FAIL" "$TOTAL"
fi
printf "═══════════════════════════════════════════\n"
echo ""

exit "$FAIL"
