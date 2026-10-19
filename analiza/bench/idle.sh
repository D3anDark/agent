#!/bin/bash
# usage: bench/idle.sh [runs=5]
# Trivial prompt, no tools: measures harness latency, own CPU, RSS and base context size.
N=${1:-5}
OUT=${OUT:-/tmp/hb-out}/idle
AGENT_BIN=${AGENT_BIN:-$HOME/developer/agent/source/target/release/agent}
P="Reply with just OK"
mkdir -p "$OUT" && cd "$(mktemp -d)" || exit 1
for ((i = 1; i <= N; i++)); do for h in agent jcode pi; do
  case $h in
    agent) cmd=("$AGENT_BIN" --json --no-session --model claude-opus-5-5 --thinking medium -p "$P");;
    jcode) cmd=(env JCODE_ANTHROPIC_REASONING_EFFORT=medium jcode run --no-update --quiet -p claude -m claude-opus-5-5 --json "$P");;
    pi)    cmd=(pi -p --no-session --mode json --model anthropic/claude-opus-5-5 --thinking medium "$P");;
  esac
  /usr/bin/time -l "${cmd[@]}" > "$OUT/$h-$i.out" 2> "$OUT/$h-$i.err" < /dev/null
  echo "$h $i real=$(rg '\breal\b' "$OUT/$h-$i.err" | awk '{print $1}') cpu=$(rg '\breal\b' "$OUT/$h-$i.err" | awk '{print $3+$5}') rss_mb=$(rg -o '(\d+)\s+maximum resident' -r '$1' "$OUT/$h-$i.err" | awk '{printf "%.0f",$1/1048576}')"
done; done
