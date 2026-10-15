#!/usr/bin/env bash
# rotate.sh BACKUP_DIR KEEP
# Deletes old backup-*.tar.gz archives in BACKUP_DIR, keeping the
# newest KEEP by modification time.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../lib/common.sh
. "$here/lib/common.sh"

[ $# -eq 2 ] || die "usage: rotate.sh BACKUP_DIR KEEP"
dir=$1
keep=$2

[ -d "$dir" ] || die "no such directory: $dir"

ls -1t "$dir"/backup-*.tar.gz 2>/dev/null | head -n "$keep" | while IFS= read -r f; do
  log "deleting $f"
  rm -f "$f"
done
