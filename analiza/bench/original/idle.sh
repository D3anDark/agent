#!/bin/bash
A=/Users/artur/developer/agent/source/target/release/agent; P="Reply with just OK"
mkdir -p /tmp/hb/idle; cd /tmp/hb/smoke
for i in 1 2 3 4 5; do for h in agent jcode pi; do
case $h in
  agent) cmd=($A --json --no-session --model claude-opus-5-5 --thinking medium -p "$P");;
  jcode) cmd=(env JCODE_ANTHROPIC_REASONING_EFFORT=medium jcode run --no-update --quiet -p claude -m claude-opus-5-5 --json "$P");;
  pi)    cmd=(pi -p --no-session --mode json --model anthropic/claude-opus-5-5 --thinking medium "$P");;
esac
/usr/bin/time -l "${cmd[@]}" > /tmp/hb/idle/$h-$i.out 2> /tmp/hb/idle/$h-$i.err < /dev/null
echo "$h $(rg '\breal\b' /tmp/hb/idle/$h-$i.err | awk '{print $1, $3+$5}') $(rg -o '(\d+)\s+maximum resident' -r '$1' /tmp/hb/idle/$h-$i.err | awk '{printf "%.0f",$1/1048576}')"
done; done
