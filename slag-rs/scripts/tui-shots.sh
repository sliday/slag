#!/usr/bin/env bash
# Render the slag terminal journey as PNG frames.
#
#   scripts/tui-shots.sh <out_dir>
#
# Plays the recorded runs in tests/fixtures/runs through `slag replay`,
# stopped at fixed moments, in both renderers (stream view and --tui
# dashboard), at 80x24, 120x36 and 160x48. Adds the screens that need no
# tape: the first run with no key, `slag key`, and `slag status`. Every
# frame is a replay held still at a named point, so the same inputs give
# the same PNGs.
#
# Output: <out_dir>/<cols>x<rows>/<renderer>-<nn>-<moment>.png
#
# Needs vhs (charm.sh). SLAG_BIN picks the binary (default
# target/debug/slag); SHOTS_SIZES narrows the sizes ("80x24 120x36").
set -euo pipefail

out=${1:?usage: tui-shots.sh <out_dir>}
here=$(cd "$(dirname "$0")/.." && pwd)
bin=${SLAG_BIN:-$here/target/debug/slag}
runs=$here/tests/fixtures/runs
command -v vhs >/dev/null || { echo "tui-shots: vhs is not installed" >&2; exit 1; }
[ -x "$bin" ] || { echo "tui-shots: no slag binary at $bin" >&2; exit 1; }
mkdir -p "$out"
out=$(cd "$out" && pwd)

# The two recorded runs: a forge with parallel anvils, retries and cracks,
# and a small tempered one that reaches the warden.
forge=$runs/textstats.tape.jsonl
temper=$runs/hello-temper.tape.jsonl
for t in "$forge" "$temper"; do
  [ -f "$t" ] || { echo "tui-shots: missing fixture $t" >&2; exit 1; }
done

# Scratch: a bin dir with `slag` on PATH, an empty config (no key), a
# config holding a demo key, and a finished project for `slag status`.
work=$(mktemp -d "${TMPDIR:-/tmp}/slag-shots.XXXX")
mkdir -p "$work/bin" "$work/nokey" "$work/withkey" "$work/project" "$work/empty"
ln -s "$bin" "$work/bin/slag"
printf 'openrouter_api_key = "sk-or-v1-demo0000000000000000000000000000000000000000000000000000c0de"\n' \
  > "$work/withkey/config.toml"
python3 - "$forge" "$work/project" <<'PY'
import json, sys
tape, dest = sys.argv[1], sys.argv[2]
plan = prd = blueprint = None
for line in open(tape):
    d = json.loads(line)
    if "plan" in d: plan = d["plan"]
    if d.get("doc") == "PRD.md": prd = d["text"]
    if d.get("doc") == "BLUEPRINT.md": blueprint = d["text"]
for name, text in (("PLAN.md", plan), ("PRD.md", prd), ("BLUEPRINT.md", blueprint)):
    if text is not None:
        open(f"{dest}/{name}", "w").write(text)
PY

# name|tape|point. Points resolve inside each tape (`slag replay --points`).
moments=(
  "01-start|$forge|start"
  "02-plan|$forge|plan"
  "03-anvils|$forge|anvils"
  "04-retry|$forge|retry"
  "05-crack|$forge|crack"
  "06-warden|$temper|warden"
  "07-assay|$temper|assay"
)

shot() { # renderer name size command [keys-to-leave]
  local renderer=$1 name=$2 dir=$3 cmd=$4 leave=$5
  cat <<EOF
Type "clear; $cmd"
Enter
Sleep 2s
Screenshot "$dir/$renderer-$name.png"
$leave
Sleep 400ms
EOF
}

for size in ${SHOTS_SIZES:-80x24 120x36 160x48}; do
  case $size in
    80x24) px="770 420" ;;
    120x36) px="1125 612" ;;
    160x48) px="1490 800" ;;
    *) echo "tui-shots: no pixel geometry for $size" >&2; exit 1 ;;
  esac
  set -- $px
  dir=$out/$size
  mkdir -p "$dir"
  tape=$work/shots-$size.tape
  {
    cat <<EOF
Output "$work/shots-$size.gif"
Set Shell "bash"
Set FontSize 14
Set Width $1
Set Height $2
Set Padding 10
Set TypingSpeed 1ms
Set CursorBlink false
Hide
Type "cd $work/empty && export PATH=$work/bin:\$PATH PS1='\$ ' COLORTERM=truecolor SLAG_CONFIG_DIR=$work/nokey && unset OPENROUTER_API_KEY && clear"
Enter
Sleep 500ms
Show
EOF
    for m in "${moments[@]}"; do
      IFS='|' read -r name t point <<<"$m"
      shot stream "$name" "$dir" "slag replay $t --at $point" "Ctrl+C"
    done
    for m in "${moments[@]}"; do
      IFS='|' read -r name t point <<<"$m"
      shot tui "$name" "$dir" "slag replay $t --tui --at $point" "Type q"
    done
    shot cli "08-nokey" "$dir" "slag 'build a tiny CLI'" "Ctrl+C"
    shot cli "09-key" "$dir" "SLAG_CONFIG_DIR=$work/withkey slag key" ""
    shot cli "10-status" "$dir" "cd $work/project && slag status; cd $work/empty" ""
  } > "$tape"
  echo "tui-shots: $size" >&2
  vhs "$tape" >/dev/null
done
echo "tui-shots: frames in $out" >&2
