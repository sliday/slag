#!/usr/bin/env bash
# bench/run.sh - issue-solving benchmark runner.
#
# Usage:
#   bench/run.sh --agent <adapter> [--tasks id1,id2] [--timeout 600] [--runs 1] [--out results.json]
#   bench/run.sh --verify-fixtures [--tasks id1,id2]
#
# An adapter is an executable taking: <issue_file> <repo_dir>.
set -u

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISSUES_DIR="$BENCH_DIR/issues"

AGENT=""
TASKS=""
TIMEOUT=600
RUNS=1
OUT=""
VERIFY=0
LEAK_CHECK=0

# Never let a caller's environment leak bench paths into adapters.
unset BENCH_TASK_DIR 2>/dev/null || true
unset BENCH_REFERENCE_PATCH 2>/dev/null || true

usage() {
  cat <<'EOF'
Usage:
  bench/run.sh --agent <adapter> [--tasks id1,id2] [--timeout 600] [--runs 1] [--out results.json]
  bench/run.sh --verify-fixtures [--tasks id1,id2]
  bench/run.sh --leak-check

Options:
  --agent        Path to adapter executable, or a name in bench/agents/.
  --tasks        Comma-separated task ids. Default: all tasks.
  --timeout      Per-task timeout in seconds. Default: 600.
  --runs         Repetitions per task. Default: 1.
  --out          JSON output path. Default: bench/results/<timestamp>/results.json.
  --verify-fixtures  Check that hidden tests fail on the untouched repo and
                 pass after reference.patch is applied. Runs the leak
                 check first. No agent is run.
  --leak-check   Run the probe adapter through the normal plumbing and
                 fail if the environment or handed-over paths leak
                 'hidden', 'reference.patch', or 'bench/issues'.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --agent) AGENT=$2; shift 2 ;;
    --tasks) TASKS=$2; shift 2 ;;
    --timeout) TIMEOUT=$2; shift 2 ;;
    --runs) RUNS=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --verify-fixtures) VERIFY=1; shift ;;
    --leak-check) LEAK_CHECK=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

discover_tasks() {
  local d
  for d in "$ISSUES_DIR"/*/; do
    [ -f "$d/meta.json" ] && basename "$d"
  done | sort
}

build_task_list() {
  TASK_LIST=""
  if [ -n "$TASKS" ]; then
    local old_ifs=$IFS t
    IFS=','
    for t in $TASKS; do
      if [ ! -d "$ISSUES_DIR/$t" ]; then
        echo "unknown task: $t" >&2
        exit 2
      fi
      TASK_LIST="$TASK_LIST $t"
    done
    IFS=$old_ifs
  else
    local t
    for t in $(discover_tasks); do
      TASK_LIST="$TASK_LIST $t"
    done
  fi
  [ -n "$TASK_LIST" ] || { echo "no tasks found in $ISSUES_DIR" >&2; exit 2; }
}

resolve_agent() {
  if [ ! -x "$AGENT" ] && [ -x "$BENCH_DIR/agents/$AGENT" ]; then
    AGENT="$BENCH_DIR/agents/$AGENT"
  fi
  if [ ! -x "$AGENT" ]; then
    echo "adapter not executable: $AGENT" >&2
    exit 2
  fi
  case "$AGENT" in
    /*) ;;
    *) AGENT="$(pwd)/$AGENT" ;;
  esac
}

# Runs "$@" under the timeout. Sets RC and TIMED_OUT.
# Uses gtimeout when available, otherwise a background watchdog.
run_with_timeout() {
  TIMED_OUT=0
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout -k 5 "$TIMEOUT" "$@"
    RC=$?
    if [ "$RC" -eq 124 ]; then
      TIMED_OUT=1
    fi
    return
  fi
  local marker="$WATCH_MARKER"
  rm -f "$marker"
  "$@" &
  local pid=$!
  (
    sleep "$TIMEOUT"
    if kill -0 "$pid" 2>/dev/null; then
      touch "$marker"
      kill -TERM "$pid" 2>/dev/null
      sleep 3
      pkill -KILL -P "$pid" 2>/dev/null
      kill -KILL "$pid" 2>/dev/null
    fi
  ) &
  local wd=$!
  wait "$pid"
  RC=$?
  kill "$wd" 2>/dev/null
  wait "$wd" 2>/dev/null
  if [ -f "$marker" ]; then
    TIMED_OUT=1
    rm -f "$marker"
  fi
}

git_commit_base() {
  git -C "$1" init -q
  git -C "$1" add -A
  git -C "$1" -c user.email=bench@localhost -c user.name=bench commit -qm "base"
}

# Copies ISSUE.md into a fresh temp dir that holds nothing else, so the
# path handed to an adapter never sits beside hidden/ or
# reference.patch. Prints the temp dir path; the caller removes it.
prepare_issue_copy() {
  local task_dir=$1 idir
  idir=$(mktemp -d -t benchissue)
  cp "$task_dir/ISSUE.md" "$idir/ISSUE.md"
  echo "$idir"
}

# Runs bench/agents/probe.sh through the same plumbing a real adapter
# gets. Fails when the environment or the handed-over paths leak the
# held-out tests or the reference fix.
leak_check() {
  local probe="$BENCH_DIR/agents/probe.sh"
  if [ ! -x "$probe" ]; then
    echo "leak check: probe adapter missing: $probe" >&2
    return 1
  fi
  set -- $TASK_LIST
  local task=$1
  local task_dir="$ISSUES_DIR/$task"
  local tmp idir log rc
  tmp=$(mktemp -d -t benchleak)
  cp -R "$task_dir/repo" "$tmp/repo"
  git_commit_base "$tmp/repo"
  idir=$(prepare_issue_copy "$task_dir")
  log="$tmp/probe.log"
  WATCH_MARKER="$log.timedout"
  unset BENCH_REFERENCE_PATCH 2>/dev/null || true
  run_with_timeout "$probe" "$idir/ISSUE.md" "$tmp/repo" >"$log" 2>&1
  rc=$RC
  rm -rf "$idir"
  if [ "$rc" -eq 0 ] && [ "$TIMED_OUT" -eq 0 ]; then
    echo "leak check: clean"
    rm -rf "$tmp"
    return 0
  fi
  echo "leak check: FAILED"
  sed 's/^/  /' "$log"
  rm -rf "$tmp"
  return 1
}

verify_fixtures() {
  local overall=0
  local task
  leak_check || overall=1
  for task in $TASK_LIST; do
    local td="$ISSUES_DIR/$task"
    echo "task: $task"
    local tmp
    tmp=$(mktemp -d -t benchverify)
    cp -R "$td/repo" "$tmp/repo"
    cp -R "$td/hidden" "$tmp/repo/hidden"

    if "$td/check.sh" "$tmp/repo" >"$tmp/baseline.log" 2>&1; then
      echo "  baseline: UNEXPECTED PASS (hidden tests must fail on the buggy repo)"
      overall=1
    else
      echo "  baseline: fails as expected"
    fi

    git_commit_base "$tmp/repo"
    if ! git -C "$tmp/repo" apply "$td/reference.patch" 2>"$tmp/apply.log"; then
      echo "  patched: reference.patch DOES NOT APPLY"
      sed 's/^/    /' "$tmp/apply.log"
      overall=1
    elif "$td/check.sh" "$tmp/repo" >"$tmp/patched.log" 2>&1; then
      echo "  patched: passes"
    else
      echo "  patched: STILL FAILS"
      tail -20 "$tmp/patched.log" | sed 's/^/    /'
      overall=1
    fi
    rm -rf "$tmp"
  done
  if [ "$overall" -eq 0 ]; then
    echo "all fixtures verified"
  else
    echo "fixture verification FAILED" >&2
  fi
  return "$overall"
}

median_p90() {
  # stdin: one wall time per line. stdout: "median p90"
  sort -n | awk '
    { a[NR] = $1 }
    END {
      if (NR == 0) { print "n/a n/a"; exit }
      if (NR % 2 == 1) { med = a[(NR + 1) / 2] } else { med = (a[NR / 2] + a[NR / 2 + 1]) / 2 }
      p = int(0.9 * NR); if (p < 0.9 * NR) p++; if (p < 1) p = 1
      printf "%.1f %d\n", med, a[p]
    }'
}

build_task_list

if [ "$LEAK_CHECK" -eq 1 ]; then
  leak_check
  exit $?
fi

if [ "$VERIFY" -eq 1 ]; then
  verify_fixtures
  exit $?
fi

if [ -z "$AGENT" ]; then
  echo "missing --agent (or use --verify-fixtures)" >&2
  usage >&2
  exit 2
fi
resolve_agent

TS=$(date +%Y%m%d-%H%M%S)
RESULTS_DIR="$BENCH_DIR/results/$TS"
mkdir -p "$RESULTS_DIR/work"
if [ -z "$OUT" ]; then
  OUT="$RESULTS_DIR/results.json"
fi
case "$OUT" in
  /*) ;;
  *) OUT="$(pwd)/$OUT" ;;
esac

REC_FILE="$RESULTS_DIR/records.tsv"
: >"$REC_FILE"

echo "agent:   $AGENT"
echo "timeout: ${TIMEOUT}s   runs: $RUNS"
echo "results: $RESULTS_DIR"
echo

for task in $TASK_LIST; do
  run=1
  while [ "$run" -le "$RUNS" ]; do
    task_dir="$ISSUES_DIR/$task"
    work="$RESULTS_DIR/work/${task}-r${run}"
    cp -R "$task_dir/repo" "$work"
    git_commit_base "$work"
    base=$(git -C "$work" rev-parse HEAD)

    log="$RESULTS_DIR/${task}.r${run}.log"
    check_log="$RESULTS_DIR/${task}.r${run}.check.log"
    WATCH_MARKER="$log.timedout"
    # Only the reference adapter may see a path into bench/issues/.
    if [ "$(basename "$AGENT")" = "reference.sh" ]; then
      export BENCH_REFERENCE_PATCH="$task_dir/reference.patch"
    else
      unset BENCH_REFERENCE_PATCH 2>/dev/null || true
    fi
    issue_dir=$(prepare_issue_copy "$task_dir")

    start=$(date +%s)
    run_with_timeout "$AGENT" "$issue_dir/ISSUE.md" "$work" >"$log" 2>&1
    end=$(date +%s)
    rm -rf "$issue_dir"
    wall=$((end - start))
    rc=$RC
    timed_out=$TIMED_OUT

    git -C "$work" add -A 2>/dev/null
    diff_stat=$(git -C "$work" diff --cached "$base" --shortstat | sed 's/^ *//')
    [ -n "$diff_stat" ] || diff_stat="no changes"

    cp -R "$task_dir/hidden" "$work/hidden"
    passed=0
    if "$task_dir/check.sh" "$work" >"$check_log" 2>&1; then
      passed=1
    fi

    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$task" "$run" "$passed" "$wall" "$rc" "$timed_out" "$diff_stat" "$log" "$check_log" >>"$REC_FILE"

    echo "  $task run $run: $([ "$passed" -eq 1 ] && echo PASS || echo FAIL) ${wall}s exit=$rc$([ "$timed_out" -eq 1 ] && echo ' (timed out)')"
    run=$((run + 1))
  done
done

echo

# JSON output.
{
  echo '{'
  echo "  \"agent\": \"$AGENT\","
  echo "  \"timestamp\": \"$TS\","
  echo "  \"timeout\": $TIMEOUT,"
  echo "  \"runs\": $RUNS,"
  echo '  "results": ['
  first=1
  while IFS='	' read -r task run passed wall rc timed_out diff_stat log check_log; do
    [ "$first" -eq 1 ] || echo ','
    first=0
    pbool=false; [ "$passed" -eq 1 ] && pbool=true
    tbool=false; [ "$timed_out" -eq 1 ] && tbool=true
    printf '    {"task": "%s", "run": %s, "passed": %s, "wall_seconds": %s, "exit_code": %s, "timed_out": %s, "diff_stat": "%s", "log": "%s", "check_log": "%s"}' \
      "$task" "$run" "$pbool" "$wall" "$rc" "$tbool" "$diff_stat" "$log" "$check_log"
  done <"$REC_FILE"
  echo
  echo '  ]'
  echo '}'
} >"$OUT"

# Summary table.
echo "task               run  result   wall(s)  exit  timed_out  diff"
echo "-----------------  ---  -------  -------  ----  ---------  ---------------------------"
total=0
passed_n=0
while IFS='	' read -r task run passed wall rc timed_out diff_stat log check_log; do
  total=$((total + 1))
  [ "$passed" -eq 1 ] && passed_n=$((passed_n + 1))
  res=FAIL; [ "$passed" -eq 1 ] && res=PASS
  to=no; [ "$timed_out" -eq 1 ] && to=yes
  printf '%-17s  %3s  %-7s  %7s  %4s  %-9s  %s\n' "$task" "$run" "$res" "$wall" "$rc" "$to" "$diff_stat"
done <"$REC_FILE"

stats=$(cut -f4 "$REC_FILE" | median_p90)
med=${stats% *}
p90=${stats#* }
echo "-----------------  ---  -------  -------  ----  ---------  ---------------------------"
echo "pass rate: $passed_n/$total   median wall: ${med}s   p90 wall: ${p90}s"
echo "json: $OUT"

[ "$passed_n" -eq "$total" ]
