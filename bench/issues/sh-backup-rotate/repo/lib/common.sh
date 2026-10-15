# Shared helpers for the backup tools.

log() {
  printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"
}

die() {
  echo "error: $*" >&2
  exit 1
}
