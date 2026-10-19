#!/usr/bin/env bash
# slag-fix adapter: runs `slag fix` on the issue in the repo.
#
# Pick the binary with SLAG_BIN (default: slag on PATH). Pick the provider
# and model with the usual env knobs (SLAG_PROVIDER, SLAG_MODEL_BASE,
# SLAG_MODEL_PLAN). SLAG_CONFIG_DIR defaults to an empty temp dir, so a
# personal config file cannot pin a model behind the bench's back.
set -u

ISSUE_FILE=$1
REPO_DIR=$2

if [ -z "${SLAG_CONFIG_DIR:-}" ]; then
  SLAG_CONFIG_DIR=$(mktemp -d -t benchslagcfg)
  export SLAG_CONFIG_DIR
fi

cd "$REPO_DIR" && exec "${SLAG_BIN:-slag}" fix --issue-file "$ISSUE_FILE"
