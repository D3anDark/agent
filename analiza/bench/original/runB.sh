#!/bin/bash
# usage: run.sh <harness> <run-id>
set -u
H=$1; ID=$2
D=/tmp/hb/runsB/$H-$ID; R=/tmp/hb/resultsB/$H-$ID
rm -rf "$D" "$R"; mkdir -p "$R" /tmp/hb/runsB; cp -R /tmp/hb/fixtureB "$D"; cd "$D" || exit 1
P='Complete the work described in TASK.md in this directory. Check with `bun test`. Do not edit files under test/. Do not ask questions; work until done.'
A=/Users/artur/developer/agent/source/target/release/agent
start=$(python3 -c 'import time;print(time.time())')
case $H in
  agent) cmd=($A --headless --yolo --no-session --model claude-opus-5-5 --thinking medium "$P");;
  jcode) cmd=(env JCODE_ANTHROPIC_REASONING_EFFORT=medium jcode run --no-update --quiet -p claude -m claude-opus-5-5 --ndjson "$P");;
  pi)    cmd=(pi -p --no-session --mode json --model anthropic/claude-opus-5-5 --thinking medium "$P");;
esac
/usr/bin/time -l timeout 1200 "${cmd[@]}" > "$R/events.jsonl" 2> "$R/stderr.txt" < /dev/null
echo $? > "$R/exit"
python3 -c "import time;print(round(time.time()-$start,2))" > "$R/wall"
git -C "$D" diff --stat HEAD -- . > "$R/diffstat"; git -C "$D" status --porcelain > "$R/status"
git -C "$D" diff HEAD -- test/ > "$R/test-tamper.diff"
cp /tmp/hb/hiddenB/grade.ts "$D/"; ls "$D" > "$R/ls"; git -C "$D" diff HEAD -- package.json > "$R/pkg.diff"
(cd "$D" && bun test > "$R/visible.txt" 2>&1; bun grade.ts > "$R/grade.txt" 2>&1)
echo "$H-$ID wall=$(cat $R/wall) exit=$(cat $R/exit) $(tail -1 "$R/grade.txt")"
