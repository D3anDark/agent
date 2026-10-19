#!/bin/bash
# prints TSV: run wall exit pass fail tools in out cread cwrite cost_usd maxrss_mb cpu_s tamper src_lines_added src_lines_removed
cd /tmp/hb/${1:-results}
for d in */; do [ -f ${d}grade.txt ] || continue; r=${d%/}; h=${r%-*}; E=$r/events.jsonl
  case $h in
    agent) u=$(jq -s '[.[]|select(.type=="usage")]|last|[.input,.output,.cache_read,.cache_write]' $E); t=$(jq -s '[.[]|select(.type=="tool")]|length' $E);;
    jcode) u=$(jq -s '[.[]|select(.type=="tokens")]|[(map(.input)|add),(map(.output)|add),(map(.cache_read_input)|add),(map(.cache_creation_input)|add)]' $E); t=$(jq -s '[.[]|select(.type=="tool_start")]|length' $E);;
    pi)    u=$(jq -s '[.[]|select(.type=="message_end" and .message.role=="assistant")|.message.usage]|[(map(.input)|add),(map(.output)|add),(map(.cacheRead)|add),(map(.cacheWrite)|add)]' $E); t=$(jq -s '[.[]|select(.type=="message_end" and .message.role=="assistant")|.message.content[]|select(.type=="toolCall")]|length' $E);;
  esac
  read i o cr cw <<<"$(jq -r 'map(.//0)|@tsv' <<<"$u")"
  cost=$(jq -n "($i*4+$o*20+$cr*0.2+$cw*5)/1e6*100|round/100")
  rss=$(rg -o '(\d+)\s+maximum resident' -r '$1' $r/stderr.txt | awk '{printf "%.0f",$1/1048576}')
  cpu=$(rg '\breal\b' $r/stderr.txt | awk '{print $3+$5}')
  if [ "${1:-results}" = results ]; then pass=$(rg -o '(\d+) pass' -r '$1' $r/grade.txt); fail=$(rg -o '(\d+) fail' -r '$1' $r/grade.txt); else pass=$(tail -1 $r/grade.txt); fail=-; fi
  tamper=$( [ -s $r/test-tamper.diff ] && echo YES || echo no )
  git -C /tmp/hb/${2:-runs}/$r add -N src; add=$(git -C /tmp/hb/${2:-runs}/$r diff --numstat HEAD -- src | awk '{a+=$1} END{print a+0}')
  del=$(git -C /tmp/hb/${2:-runs}/$r diff --numstat HEAD -- src | awk '{a+=$2} END{print a+0}')
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t+%s/-%s\n" $r $(cat $r/wall) $(cat $r/exit) "$pass" "$fail" $t $i $o $cr $cw $cost $rss $cpu $tamper $add $del
done
