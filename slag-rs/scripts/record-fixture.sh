#!/usr/bin/env bash
# Record a real slag run as a replay fixture.
#
#   scripts/record-fixture.sh <name> "<commission>" [slag flags...]
#
# Runs slag on a fresh scratch project inside a private tmux server (a
# real terminal, 120x36), then copies the run's tape into
# tests/fixtures/runs/<name>.tape.jsonl with the scratch path and $HOME
# scrubbed. The raw terminal bytes land beside it as <name>.terminal.log,
# which is the ground truth a replay is compared against.
#
# Needs OPENROUTER_API_KEY. Models default to cheap, fast ones; override
# with SLAG_MODEL_BASE / SLAG_MODEL_PLAN / SLAG_MODEL_JUDGE. Spend is
# capped by SLAG_MAX_COST_RUN (default 0.45 dollars).
set -euo pipefail

name=${1:?usage: record-fixture.sh <name> "<commission>" [flags...]}
commission=${2:?usage: record-fixture.sh <name> "<commission>" [flags...]}
shift 2

here=$(cd "$(dirname "$0")/.." && pwd)
bin=${SLAG_BIN:-$here/target/debug/slag}
out=$here/tests/fixtures/runs
[ -x "$bin" ] || { echo "no slag binary at $bin (cargo build first)" >&2; exit 1; }
[ -n "${OPENROUTER_API_KEY:-}" ] || { echo "OPENROUTER_API_KEY is not set" >&2; exit 1; }

scratch=${FIXTURE_WORK:-${TMPDIR:-/tmp}}
mkdir -p "$scratch"
work=$(mktemp -d "$scratch/slag-fixture-$name.XXXX")
real=$(cd "$work" && pwd -P)
mkdir -p "$work/.cfg"
(cd "$work" && git init -q && git commit -q --allow-empty -m init)

export SLAG_CONFIG_DIR="$work/.cfg"
export SLAG_MODEL_BASE=${SLAG_MODEL_BASE:-qwen/qwen3-coder-30b-a3b-instruct}
export SLAG_MODEL_PLAN=${SLAG_MODEL_PLAN:-anthropic/claude-haiku-5.5}
export SLAG_MODEL_JUDGE=${SLAG_MODEL_JUDGE:-anthropic/claude-haiku-5.5}
export SLAG_MODEL_ALT=${SLAG_MODEL_ALT:-$SLAG_MODEL_BASE}
export SLAG_DUEL=${SLAG_DUEL:-off}
export SLAG_MAX_COST_RUN=${SLAG_MAX_COST_RUN:-0.45}
export COLORTERM=truecolor

# A private tmux server inherits this environment, key included, without
# the key ever appearing on a command line.
sock=slag-fixture-$$
args=$(printf '%q ' "$bin" "$commission" "$@")
tmux -L "$sock" new-session -d -s rec -x 120 -y 36 -c "$work" \
  "$args; echo \$? > '$work/.exit'"
tmux -L "$sock" pipe-pane -o -t rec "cat > '$work/.terminal.log'"
echo "recording $name in $work" >&2
while [ ! -f "$work/.exit" ]; do sleep 2; done
code=$(cat "$work/.exit")
tmux -L "$sock" capture-pane -p -e -t rec > "$work/.last-frame.ansi" || true
tmux -L "$sock" kill-server 2>/dev/null || true

tape=$(ls -t "$work"/logs/tape-*.jsonl 2>/dev/null | head -1)
[ -n "$tape" ] || { echo "the run left no tape (exit $code)" >&2; exit 1; }
mkdir -p "$out"
scrub() {
  sed -e "s#$real#/work/$name#g" -e "s#$work#/work/$name#g" -e "s#$HOME#~#g" "$1"
}
scrub "$tape" > "$out/$name.tape.jsonl"
scrub "$work/.terminal.log" > "$out/$name.terminal.log"
echo "slag exited $code; tape: $out/$name.tape.jsonl ($(wc -l < "$out/$name.tape.jsonl") lines)" >&2
