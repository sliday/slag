#!/usr/bin/env bash
# backup.sh SOURCE_DIR DEST_DIR
# Creates a timestamped tar.gz of SOURCE_DIR inside DEST_DIR and prints
# the archive path.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../lib/common.sh
. "$here/lib/common.sh"

[ $# -eq 2 ] || die "usage: backup.sh SOURCE_DIR DEST_DIR"
src=$1
dest=$2

[ -d "$src" ] || die "no such directory: $src"
mkdir -p "$dest"

ts=$(date '+%Y%m%d-%H%M%S')
out="$dest/backup-$ts.tar.gz"
tar -czf "$out" -C "$src" .
log "created $out"
echo "$out"
