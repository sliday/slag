#!/usr/bin/env bash
# fx adapter: runs the fx CLI non-interactively on the issue.
set -u

ISSUE_FILE=$1
REPO_DIR=$2

PROMPT="$(cat "$ISSUE_FILE")

---

You are working in the repository at the current working directory. Fix the issue described above. Run the repository's existing test suite to check your work. Do not ask questions; make the change directly."

cd "$REPO_DIR" && exec fx ask --no-save --no-color "$PROMPT"
