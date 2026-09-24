#!/usr/bin/env bash
# Experiment 2, Linux arm: after one warm pass, launch N concurrent LMDB readers over
# the same environment and record aggregate minor faults + global PageTables delta.
# Counterpart of `elephance launch` on the Twizzler side. Run with THP noted; repeat
# with THP disabled (echo never > /sys/kernel/mm/transparent_hugepage/enabled).
#
# Usage: NS="1 2 4 8 16" QUERIES=1000000 LMDB=/tmp/elephance/lmdb-16000000 ./linux-nreaders.sh
set -euo pipefail

NS="${NS:-1 2 4 8 16}"
QUERIES="${QUERIES:-1000000}"
LMDB="${LMDB:?set LMDB to the lmdb environment dir}"
BIN="${BIN:-$(dirname "$0")/../linux/target/release/elephance-linux}"
OUT="${OUT:-elephance-linux-nreaders.$(date +%Y%m%d-%H%M%S).log}"

echo "# elephance linux nreaders ns=[$NS] queries=$QUERIES lmdb=$LMDB thp=$(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo unknown)" | tee -a "$OUT"

# Warm pass: fault the working set into page cache once, so the sweep below measures
# sharing behavior rather than disk.
"$BIN" query-lmdb --path "$LMDB" --queries "$QUERIES" | tee -a "$OUT"

pt_kb() { awk '/^PageTables:/ {print $2}' /proc/meminfo; }

for n in $NS; do
    pt0=$(pt_kb)
    t0=$(date +%s%N)
    pids=()
    for i in $(seq 0 $((n - 1))); do
        "$BIN" query-lmdb --path "$LMDB" --queries "$QUERIES" --id "$i" >>"$OUT" &
        pids+=($!)
    done
    fail=0
    for p in "${pids[@]}"; do wait "$p" || fail=$((fail + 1)); done
    t1=$(date +%s%N)
    pt1=$(pt_kb)
    echo "ELPH os=linux role=launch arm=lmdb n=$n failures=$fail wall_us=$(((t1 - t0) / 1000)) global_pagetables_delta_kb=$((pt1 - pt0))" | tee -a "$OUT"
done
echo "wrote $OUT"
