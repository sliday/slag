#!/usr/bin/env bash
# reference adapter: applies the task's reference patch.
# The runner sets BENCH_REFERENCE_PATCH only for this adapter (it checks
# the adapter basename); no other adapter receives a path into
# bench/issues/. Instrument check: must score 8/8.
set -eu

ISSUE_FILE=$1
REPO_DIR=$2

: "${BENCH_REFERENCE_PATCH:?BENCH_REFERENCE_PATCH is not set; run via bench/run.sh}"

git -C "$REPO_DIR" apply "$BENCH_REFERENCE_PATCH"
