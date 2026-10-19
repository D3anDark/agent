#!/bin/bash
# usage: bench/all.sh [runs=3]
# Rotates harness order per round so no harness always goes first.
DIR=$(dirname "$0")
N=${1:-3}
orders=("agent jcode pi" "jcode pi agent" "pi agent jcode")
for TASK in A B; do
  for ((r = 1; r <= N; r++)); do
    for h in ${orders[$(((r - 1) % 3))]}; do "$DIR/run.sh" $TASK $h $r; done
  done
done
