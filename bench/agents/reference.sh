#!/usr/bin/env bash
# reference adapter: applies the task's reference.patch.
# The runner exports BENCH_TASK_DIR so this adapter can find the patch.
# Instrument check: must score 8/8.
set -eu

ISSUE_FILE=$1
REPO_DIR=$2

: "${BENCH_TASK_DIR:?BENCH_TASK_DIR is not set; run via bench/run.sh}"

git -C "$REPO_DIR" apply "$BENCH_TASK_DIR/reference.patch"
