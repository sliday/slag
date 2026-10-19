#!/usr/bin/env bash
# slag-fix adapter: runs `slag fix` on the issue in the repo.
#
# Pick the binary with SLAG_BIN (default: slag on PATH). Pick the provider
# and model with the usual env knobs (SLAG_PROVIDER, SLAG_MODEL_BASE,
# SLAG_MODEL_PLAN). SLAG_CONFIG_DIR defaults to an empty temp dir, so a
# personal config file cannot pin a model behind the bench's back.
#
# By default the adapter runs `slag fix` exactly as a user would, so
# numbers stay comparable across rounds. Stream baselines opt in to more:
# SLAG_FIX_TRIES sets --tries (a baseline uses 1 to measure the base model,
# not retries on the plan model), SLAG_FIX_NO_COMMIT=1 adds --no-commit,
# and SLAG_FIX_ARGS appends extra flags.
set -u

ISSUE_FILE=$1
REPO_DIR=$2

if [ -z "${SLAG_CONFIG_DIR:-}" ]; then
  SLAG_CONFIG_DIR=$(mktemp -d -t benchslagcfg)
  export SLAG_CONFIG_DIR
fi

args=(fix --issue-file "$ISSUE_FILE")
if [ -n "${SLAG_FIX_TRIES:-}" ]; then
  args+=(--tries "$SLAG_FIX_TRIES")
fi
if [ "${SLAG_FIX_NO_COMMIT:-}" = "1" ]; then
  args+=(--no-commit)
fi
# shellcheck disable=SC2206
extra=(${SLAG_FIX_ARGS:-})

cd "$REPO_DIR" && exec "${SLAG_BIN:-slag}" "${args[@]}" ${extra[@]+"${extra[@]}"}
