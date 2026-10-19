#!/bin/bash
for r in 1 2 3; do for h in agent jcode pi; do /tmp/hb/run.sh $h $r; done; done
