#!/usr/bin/env bash
# slag-fix adapter: runs `slag fix` on the task.
#
# SLAG_BIN picks the binary (default: slag from PATH). SLAG_PROVIDER and
# SLAG_MODEL_BASE / SLAG_MODEL_PLAN configure the models; slag reads them
# from the environment itself. The fix stays in the working tree
# (--no-commit) so the runner scores the diff. SLAG_FIX_TRIES overrides
# the attempt count (default: 1, so a baseline measures the base model,
# not retries on the plan model). SLAG_FIX_ARGS appends extra flags.
set -u

ISSUE_FILE=$1
REPO_DIR=$2

SLAG=${SLAG_BIN:-slag}
TRIES=${SLAG_FIX_TRIES:-1}

cd "$REPO_DIR" && exec "$SLAG" fix --issue-file "$ISSUE_FILE" --no-commit --tries "$TRIES" ${SLAG_FIX_ARGS:-}
