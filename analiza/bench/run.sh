#!/bin/bash
# usage: bench/run.sh <A|B> <agent|jcode|pi> <run-id>
# Copies the task fixture to $WORK, runs one harness headless, then grades with the hidden checks.
set -u
TASK=$1; H=$2; ID=$3
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${WORK:-/tmp/hb-work}
OUT=${OUT:-/tmp/hb-out}
T=$ROOT/tasks/$TASK
D=$WORK/$TASK/$H-$ID; R=$OUT/$TASK/$H-$ID
AGENT_BIN=${AGENT_BIN:-$HOME/developer/agent/source/target/release/agent}
MODEL=claude-opus-5-5

rm -rf "$D" "$R"; mkdir -p "$R" "$(dirname "$D")"
cp -R "$T/fixture" "$D"; cd "$D" || exit 1
git init -q && git add -A && git -c user.name=bench -c user.email=bench@local commit -qm init
BASE=$(git rev-parse HEAD)

P='Complete the work described in TASK.md in this directory. Check with `bun test`. Do not edit files under test/. Do not ask questions; work until done.'
# PROFILE=clean runs each harness with only its login, from the dirs bench/clean-homes.sh builds.
C=; [ "${PROFILE:-real}" = clean ] && C=/tmp/bench-clean
case $H in
  agent) cmd=(env ${C:+HOME=$C/agent} "$AGENT_BIN" --headless --yolo --no-session --model $MODEL --thinking medium "$P");;
  jcode) cmd=(env ${C:+JCODE_HOME=$C/jcode} JCODE_ANTHROPIC_REASONING_EFFORT=medium jcode run --no-update --quiet -p claude -m $MODEL --ndjson "$P");;
  pi)    cmd=(env ${C:+PI_CODING_AGENT_DIR=$C/pi} pi -p --no-session --mode json --model anthropic/$MODEL --thinking medium "$P");;
  *) echo "unknown harness $H"; exit 2;;
esac

start=$(python3 -c 'import time;print(time.time())')
/usr/bin/time -l timeout 1200 "${cmd[@]}" > "$R/events.jsonl" 2> "$R/stderr.txt" < /dev/null
echo $? > "$R/exit"
python3 -c "import time;print(round(time.time()-$start,2))" > "$R/wall"

# Diff against the init commit, not HEAD: jcode commits its own work.
git diff --stat "$BASE" -- . > "$R/diffstat"; git status --porcelain > "$R/status"
git diff "$BASE" -- test/ > "$R/test-tamper.diff"
git diff "$BASE" -- package.json > "$R/pkg.diff"
git add -N . && git diff "$BASE" -- . > "$R/final.patch"

if [ "$TASK" = A ]; then
  cp "$T/hidden.test.ts" test/
  bun test > "$R/grade.txt" 2>&1
  echo "A $H-$ID wall=$(cat "$R/wall") exit=$(cat "$R/exit") $(rg -o '\d+ pass|\d+ fail' "$R/grade.txt" | tr '\n' ' ')"
else
  cp "$T/grade.ts" "$T/cases.json" .
  bun test > "$R/visible.txt" 2>&1
  bun grade.ts > "$R/grade.txt" 2>&1
  echo "B $H-$ID wall=$(cat "$R/wall") exit=$(cat "$R/exit") $(tail -1 "$R/grade.txt")"
fi
