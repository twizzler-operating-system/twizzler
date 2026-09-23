#!/bin/bash
# Aggregate per-arm sysbench results. Prints, per op and arm: n rounds, mean of round means,
# and the round-to-round spread (max-min as % of mean) -- the spread is the thing that says
# whether a between-arm difference is resolvable at all.
cd /scratch/dbittman/review/twizzler || exit 1
ARMS="${*:-base alloff nosecctx norng noshard}"
OPS='page_fault_soft_contended|page_fault_zero_fill_contended|object_create_delete|object_create_delete_contended|object_create_delete_nomap|pager_sync_dirty_page_contended|pager_create_delete_persistent'

for tag in $ARMS; do
  d="target/results/many-$tag"
  [ -d "$d" ] || { echo "MISSING arm: $tag"; continue; }
  ids=$(grep -o "build [0-9a-f]*" "$d/driver.log" 2>/dev/null | sort -u | wc -l)
  [ "$ids" -gt 1 ] && echo "!! arm $tag has $ids distinct build ids -- NOT comparable, discard"
  for f in "$d"/round*.log; do
    [ -e "$f" ] || continue
    r=$(basename "$f" | sed 's/round\([0-9]*\).*/\1/')
    ./bench-parse.sh "$f" "$tag" "$r"
  done
done | awk -F'\t' -v ops="$OPS" '
  $3 ~ ops { key=$3"\t"$1; n[key]++; s[key]+=$4; if($5!="") sp[key]+=$5;
             if(!(key in mn)||$4<mn[key]) mn[key]=$4;
             if(!(key in mx)||$4>mx[key]) mx[key]=$4 }
  END{ for(k in n){ split(k,a,"\t"); m=s[k]/n[k];
         printf "%-38s %-10s %4d %12.1f %8.1f%% %9.1f%%\n",a[1],a[2],n[k],m,(m>0?100*(mx[k]-mn[k])/m:0),(m>0?100*(sp[k]/n[k])/m:0) } }' \
  | sort | awk 'BEGIN{printf "%-38s %-10s %4s %12s %9s %10s\n","op","arm","n","mean_ns","rnd_spr%","within%"}{print}' 
