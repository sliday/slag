#!/usr/bin/env bash
# slag-fix adapter: placeholder for the upcoming `slag fix` command.
#
# TODO: `slag fix` does not exist yet and its interface may change.
# Assumed interface: cd <repo_dir> && slag fix --issue-file <issue_file>.
# Revisit this adapter when the real command lands.
set -u

ISSUE_FILE=$1
REPO_DIR=$2

cd "$REPO_DIR" && exec slag fix --issue-file "$ISSUE_FILE"
