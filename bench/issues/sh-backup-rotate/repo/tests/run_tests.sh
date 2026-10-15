#!/usr/bin/env bash
# Visible test runner. Exercises backup creation only.
set -uo pipefail

cd "$(dirname "$0")/.."

failures=0

check() {
  local name=$1
  shift
  if "$@" >/dev/null 2>&1; then
    echo "ok - $name"
  else
    echo "FAIL - $name"
    failures=$((failures + 1))
  fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/src" "$tmp/dest"
echo "hello" >"$tmp/src/file.txt"

check "backup creates an archive" bash -c "
  out=\$(bash bin/backup.sh '$tmp/src' '$tmp/dest' 2>/dev/null | tail -1)
  [ -f \"\$out\" ]
"

check "backup name carries a timestamp" bash -c "
  out=\$(bash bin/backup.sh '$tmp/src' '$tmp/dest' 2>/dev/null | tail -1)
  basename \"\$out\" | grep -qE '^backup-[0-9]{8}-[0-9]{6}\.tar\.gz$'
"

check "archive contains the source file" bash -c "
  out=\$(bash bin/backup.sh '$tmp/src' '$tmp/dest' 2>/dev/null | tail -1)
  tar -tzf \"\$out\" | grep -q 'file.txt'
"

check "missing source dir is an error" bash -c "
  ! bash bin/backup.sh '$tmp/nope' '$tmp/dest' >/dev/null 2>&1
"

if [ "$failures" -gt 0 ]; then
  echo "$failures test(s) failed"
  exit 1
fi
echo "all tests passed"
