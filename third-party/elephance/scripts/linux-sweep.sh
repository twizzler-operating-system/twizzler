#!/usr/bin/env bash
# Experiment 1, Linux arms: build both representations at each size, then time
# cold-cache opens+queries. Run on the measurement guest/host, ideally with the box
# otherwise idle. Cold cache needs root (drop_caches); without it, results are
# warm-cache and must be labeled as such.
#
# Usage: SIZES="1000000 10000000" QUERIES=1000000 DATADIR=/tmp/elephance ./linux-sweep.sh
set -euo pipefail

SIZES="${SIZES:-1000000 4000000 16000000}"
QUERIES="${QUERIES:-1000000}"
DATADIR="${DATADIR:-/tmp/elephance}"
ROUNDS="${ROUNDS:-3}"
BIN="${BIN:-$(dirname "$0")/../linux/target/release/elephance-linux}"
OUT="${OUT:-elephance-linux-sweep.$(date +%Y%m%d-%H%M%S).log}"

mkdir -p "$DATADIR"
echo "# elephance linux sweep sizes=[$SIZES] queries=$QUERIES rounds=$ROUNDS thp=$(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo unknown)" | tee -a "$OUT"

drop_caches() {
    sync
    if [ -w /proc/sys/vm/drop_caches ]; then
        echo 3 > /proc/sys/vm/drop_caches
    elif command -v sudo >/dev/null && sudo -n true 2>/dev/null; then
        echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
    else
        echo "# WARN: cannot drop caches; this round is warm-cache" | tee -a "$OUT"
    fi
}

for n in $SIZES; do
    flat="$DATADIR/flat-$n.elph"
    ldir="$DATADIR/lmdb-$n"
    [ -f "$flat" ] || "$BIN" build-flat --path "$flat" --entries "$n" | tee -a "$OUT"
    [ -d "$ldir" ] || "$BIN" build-lmdb --path "$ldir" --entries "$n" | tee -a "$OUT"
    for r in $(seq 1 "$ROUNDS"); do
        echo "# size=$n round=$r" | tee -a "$OUT"
        drop_caches
        "$BIN" query-flat --path "$flat" --queries "$QUERIES" | tee -a "$OUT"
        drop_caches
        "$BIN" query-lmdb --path "$ldir" --queries "$QUERIES" | tee -a "$OUT"
    done
done
echo "wrote $OUT"
