#!/usr/bin/env bash
# probe adapter: instrument self-check. The runner invokes it like any
# adapter: <issue_file> <repo_dir>. It fails when the environment or the
# paths it was handed leak the location of held-out tests or reference
# fixes ('hidden', 'reference.patch', 'bench/issues').
set -u

ISSUE_FILE=$1
REPO_DIR=$2
fail=0

flag() {
  echo "LEAK: $1"
  fail=1
}

# 1. Environment variables.
leaky_env=$(env | grep -E 'hidden|reference\.patch|bench/issues' || true)
if [ -n "$leaky_env" ]; then
  flag "environment variable leaks a bench path:"
  echo "$leaky_env" | sed 's/^/    /'
fi

# 2. Path strings handed to the adapter (covers every parent component,
#    since they are all inside the string).
check_path() {
  local what=$1 p=$2
  case "$p" in
    *hidden*|*reference.patch*|*bench/issues*)
      flag "$what path leaks: $p" ;;
  esac
}

# 3. Entries of the directories the runner controls: the issue file
#    must sit alone in its temp dir (the original leak had ISSUE.md
#    beside hidden/ and reference.patch), and the repo dir must not
#    contain held-out material while the agent runs.
check_entries() {
  local what=$1 dir=$2 entry base
  [ -d "$dir" ] || return 0
  for entry in "$dir"/*; do
    base=${entry##*/}
    case "$base" in
      hidden|reference.patch)
        flag "$what contains a leaking entry: $entry" ;;
    esac
  done
}

check_path "issue file" "$ISSUE_FILE"
check_path "repo dir" "$REPO_DIR"
check_entries "issue dir" "$(dirname "$ISSUE_FILE")"
check_entries "repo dir" "$REPO_DIR"
check_entries "repo parent" "$(dirname "$REPO_DIR")"

if [ "$fail" -eq 0 ]; then
  echo "no leaks found"
  exit 0
fi
exit 1
