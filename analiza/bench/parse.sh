#!/bin/bash
# usage: bench/parse.sh <results-dir>   e.g. bench/parse.sh results/B
# One TSV row per run, then one average row per harness.
# Cost is the API-equivalent price pi reports for claude-opus-5-5 per 1M tokens:
# input $4, output $20, cache read $0.20, cache write $5.
cd "$1" || exit 1
{
printf "run\twall_s\texit\tgrade\ttools\tin\tout\tcache_read\tcache_write\tcost_usd\tmax_rss_mb\tcpu_s\ttest_tamper\tsrc_added\tsrc_removed\n"
for d in */; do
  r=${d%/}; h=${r%-*}; E=$r/events.jsonl
  [ -f "$r/grade.txt" ] || continue
  case $h in
    # agent emits one cumulative usage event at the end
    agent) u=$(jq -s '[.[]|select(.type=="usage")]|last|[.input,.output,.cache_read,.cache_write]' "$E")
           t=$(jq -s '[.[]|select(.type=="tool")]|length' "$E");;
    # jcode and pi emit usage per model response
    jcode) u=$(jq -s '[.[]|select(.type=="tokens")]|[(map(.input)|add),(map(.output)|add),(map(.cache_read_input)|add),(map(.cache_creation_input)|add)]' "$E")
           t=$(jq -s '[.[]|select(.type=="tool_start")]|length' "$E");;
    pi)    u=$(jq -s '[.[]|select(.type=="message_end" and .message.role=="assistant")|.message.usage]|[(map(.input)|add),(map(.output)|add),(map(.cacheRead)|add),(map(.cacheWrite)|add)]' "$E")
           t=$(jq -s '[.[]|select(.type=="message_end" and .message.role=="assistant")|.message.content[]|select(.type=="toolCall")]|length' "$E");;
  esac
  read -r i o cr cw <<<"$(jq -r 'map(.//0)|@tsv' <<<"$u")"
  cost=$(jq -n "($i*4+$o*20+$cr*0.2+$cw*5)/1e6*100|round/100")
  rss=$(rg -o '(\d+)\s+maximum resident' -r '$1' "$r/stderr.txt" | awk '{printf "%.0f",$1/1048576}')
  cpu=$(rg '\breal\b' "$r/stderr.txt" | awk '{print $3+$5}')
  if rg -q 'score' "$r/grade.txt"; then grade=$(tail -1 "$r/grade.txt" | awk '{print $2}')
  else p=$(rg -o '(\d+) pass' -r '$1' "$r/grade.txt"); f=$(rg -o '(\d+) fail' -r '$1' "$r/grade.txt"); grade="$p/$((p + f))"; fi
  tamper=$([ -s "$r/test-tamper.diff" ] && echo YES || echo no)
  # -C /: inside a repo, git apply only counts paths under the current directory
  read -r add del <<<"$(git -C / apply --numstat "$PWD/$r/final.patch" | awk '$3 ~ /^src\// {a+=$1; d+=$2} END {print a+0, d+0}')"
  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
    "$r" "$(cat "$r/wall")" "$(cat "$r/exit")" "$grade" "$t" "$i" "$o" "$cr" "$cw" "$cost" "$rss" "$cpu" "$tamper" "$add" "$del"
done
} | tee /dev/stderr | awk -F'\t' 'NR > 1 {
    h = $1; sub(/-[0-9]+$/, "", h); n[h]++; w[h] += $2; t[h] += $5; o[h] += $7; tok[h] += $6 + $7 + $8 + $9; c[h] += $10; a[h] += $14
  }
  END { for (h in n) printf "avg %s\tn=%d\twall=%.1f\ttools=%.1f\tout=%.0f\ttokens=%.0f\tcost=$%.2f\tsrc_added=%.0f\n", h, n[h], w[h]/n[h], t[h]/n[h], o[h]/n[h], tok[h]/n[h], c[h]/n[h], a[h]/n[h] }' | sort
