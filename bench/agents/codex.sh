#!/usr/bin/env bash
# codex adapter: runs the Codex CLI non-interactively on the issue.
set -u

ISSUE_FILE=$1
REPO_DIR=$2

PROMPT="$(cat "$ISSUE_FILE")

---

You are working in the repository at the current working directory. Fix the issue described above. Run the repository's existing test suite to check your work. Do not ask questions; make the change directly."

exec codex exec -C "$REPO_DIR" -s workspace-write --skip-git-repo-check \
  -m "${CODEX_MODEL:-gpt-6.1-sol}" \
  -c model_reasoning_effort="${CODEX_EFFORT:-medium}" \
  "$PROMPT"
