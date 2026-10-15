#!/usr/bin/env bash
# Hidden test: rotation must keep the NEWEST archives.
set -uo pipefail

failures=0

check() {
  local name=$1
  shift
  if "$@"; then
    echo "ok - $name"
  else
    echo "FAIL - $name"
    failures=$((failures + 1))
  fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for n in 1 2 3 4 5; do
  f="$tmp/backup-2026010${n}-00000${n}.tar.gz"
  tar -czf "$f" --files-from /dev/null
  touch -t "2026010${n}0000.0${n}" "$f"
done

bash ./bin/rotate.sh "$tmp" 2 >/dev/null

check "newest backup survives" test -f "$tmp/backup-20260105-000005.tar.gz"
check "second newest survives" test -f "$tmp/backup-20260104-000004.tar.gz"
check "oldest is deleted" bash -c "! test -e '$tmp/backup-20260101-000001.tar.gz'"
check "second oldest is deleted" bash -c "! test -e '$tmp/backup-20260102-000002.tar.gz'"
check "middle one is deleted" bash -c "! test -e '$tmp/backup-20260103-000003.tar.gz'"
check "exactly two remain" bash -c "[ \$(ls -1 '$tmp'/backup-*.tar.gz | wc -l) -eq 2 ]"

if [ "$failures" -gt 0 ]; then
  echo "$failures test(s) failed"
  exit 1
fi
echo "all tests passed"
