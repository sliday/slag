#!/usr/bin/env bash
# Render the slag terminal journey as PNG frames.
#
#   scripts/tui-shots.sh <out_dir>
#
# Plays the recorded runs in tests/fixtures/runs through `slag replay`,
# stopped at fixed moments, in both renderers (stream view and --tui
# dashboard), at 80x24, 120x36 and 160x48. Adds the screens that need no
# tape: the first run with no key, `slag key`, and `slag status`. Every
# replay frame is held still at a named point, so the same inputs give the
# same PNGs. Each shot waits for the text that proves the frame is drawn,
# not for a fixed time: a long replay can take xterm.js seconds to paint.
#
# With SHOTS_FIX=1 it also films a live `slag fix` on the same tiny Python
# project and task the Codex CLI and Claude Code reference frames used
# (SHOTS_FIX_PROJECT, a git repo with one failing test): idle just after
# launch, working, and done, at 80x24 and 160x48. That run calls a model:
# it needs MISTRAL_API_KEY (or SHOTS_FIX_PROVIDER's key) in the
# environment, never prints it, and costs a few cents.
#
# Output: <out_dir>/<cols>x<rows>/<renderer>-<nn>-<moment>.png
#
# The terminal matches the reference captures: vhs, FontSize 16, no
# padding, the Dracula theme. Needs vhs (charm.sh); ImageMagick, when
# present, shrinks the PNGs (SHOTS_QUANTIZE=0 keeps them raw). SLAG_BIN
# picks the binary (default target/debug/slag); SHOTS_SIZES narrows the
# sizes ("80x24 160x48").
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
temper=$runs/hello-goal.tape.jsonl
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

# What proves each kind of frame is fully drawn.
held='run [0-9]+:[0-9][0-9]'      # a held stream frame draws its footer last
assay='work .*logs'                # the assay's last evidence row (no slash: vhs delimits with /)
dash='steer|new commission'        # the dashboard's key row

shot() { # renderer name dir command wait-regex keys-to-leave
  local renderer=$1 name=$2 dir=$3 cmd=$4 wait=$5 leave=$6
  cat <<EOF
Type "clear; $cmd"
Enter
Wait+Screen@20s /$wait/
Sleep 700ms
Screenshot "$dir/$renderer-$name.png"
Sleep 600ms
$leave
Sleep 400ms
EOF
}

header() { # width height
  cat <<EOF
Set Shell "bash"
Set FontSize 16
Set Width $1
Set Height $2
Set Padding 0
Set Theme "Dracula"
Set TypingSpeed 1ms
Set CursorBlink false
Set WaitTimeout 30s
EOF
}

geometry() { # cols x rows → vhs pixels at FontSize 16, measured with tput
  case $1 in
    80x24) echo "826 466" ;;
    120x36) echo "1228 698" ;;
    160x48) echo "1650 930" ;;
    *) echo "tui-shots: no pixel geometry for $1" >&2; return 1 ;;
  esac
}

for size in ${SHOTS_SIZES:-80x24 120x36 160x48}; do
  px=$(geometry "$size")
  set -- $px
  dir=$out/$size
  mkdir -p "$dir"
  tape=$work/shots-$size.tape
  {
    echo "Output \"$work/shots-$size.gif\""
    header "$1" "$2"
    cat <<EOF
Hide
Type "cd $work/empty && export PATH=$work/bin:\$PATH PS1='\$ ' COLORTERM=truecolor SLAG_CONFIG_DIR=$work/nokey && unset OPENROUTER_API_KEY && clear"
Enter
Sleep 500ms
Show
EOF
    for m in "${moments[@]}"; do
      IFS='|' read -r name t point <<<"$m"
      wait=$held
      [ "$point" = assay ] && wait=$assay
      # The replay holds the frame, then exits by itself. It prints only
      # what fits the screen: vhs's Wait+Screen reads the first rows of
      # the buffer, so a frame that scrolled would never match.
      shot stream "$name" "$dir" "slag replay $t --at $point --hold 3" "$wait" "Sleep 2500ms"
    done
    for m in "${moments[@]}"; do
      IFS='|' read -r name t point <<<"$m"
      shot tui "$name" "$dir" "slag replay $t --tui --at $point" "$dash" "Type q"
    done
    shot cli "08-nokey" "$dir" "slag 'build a tiny CLI'" "key ›" "Enter"
    shot cli "09-key" "$dir" "SLAG_CONFIG_DIR=$work/withkey slag key" "duel" ""
    shot cli "10-status" "$dir" "cd $work/project && slag status; cd $work/empty" "next" ""
  } > "$tape"
  echo "tui-shots: $size" >&2
  vhs "$tape" >"$work/shots-$size.log" 2>&1 || {
    echo "tui-shots: vhs failed on $size: $(grep -m1 -E 'timeout|error' "$work/shots-$size.log")" >&2
    exit 1
  }
done

# A live `slag fix`, framed like the references: same project, same task,
# same terminal. Idle just after launch, working once a tool has run, done
# once the prompt returns under the report card. vhs's Wait+Screen reads the
# first rows of the buffer, not the rows on screen, so it only works before
# the output scrolls; the done frame waits on the cursor line instead.
if [ "${SHOTS_FIX:-0}" = 1 ]; then
  project=${SHOTS_FIX_PROJECT:-}
  provider=${SHOTS_FIX_PROVIDER:-mistral}
  [ -d "$project/.git" ] || { echo "tui-shots: SHOTS_FIX_PROJECT must be a git repo" >&2; exit 1; }
  for size in 80x24 160x48; do
    px=$(geometry "$size")
    set -- $px
    dir=$out/$size
    mkdir -p "$dir"
    repo=$work/fix-$size
    cp -R "$project" "$repo"
    tape=$work/fix-$size.tape
    {
      echo "Output \"$work/fix-$size.gif\""
      header "$1" "$2"
      cat <<EOF
Set WaitTimeout 300s
Hide
Type "cd $repo && export PATH=$work/bin:\$PATH PS1='\$ ' COLORTERM=truecolor SLAG_CONFIG_DIR=$work/nokey && clear"
Enter
Sleep 500ms
Show
Type "slag fix 'run the tests and fix the failing one' --provider $provider"
Enter
Wait+Screen@20s /issue/
Sleep 300ms
Screenshot "$dir/fix-01-idle.png"
Wait+Screen@240s /✓ (read|run|edit|search)/
Sleep 1s
Screenshot "$dir/fix-02-working.png"
Wait+Line@300s /^\\\$\$/
Sleep 1s
Screenshot "$dir/fix-03-done.png"
Sleep 600ms
EOF
    } > "$tape"
    echo "tui-shots: slag fix at $size" >&2
    vhs "$tape" >"$work/fix-$size.log" 2>&1 || {
      echo "tui-shots: vhs failed on the $size fix: $(grep -m1 -E 'timeout|error' "$work/fix-$size.log")" >&2
      exit 1
    }
  done
fi

# Terminal frames carry few real colours; 96 keeps the anti-aliasing and
# cuts each PNG to about a third. Same input, same output: deterministic.
if command -v magick >/dev/null && [ "${SHOTS_QUANTIZE:-1}" = 1 ]; then
  for f in "$out"/*/*.png; do
    magick "$f" +dither -colors 96 -strip -define png:compression-level=9 "$f"
  done
fi
echo "tui-shots: frames in $out" >&2
