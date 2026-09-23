#!/bin/bash
# $1=log $2=config $3=round -> "config round op mean spread"
# The second strip is `\x1b([A-Z]`, NOT a literal `(B`. libtest colours the word `bench`, and the
# reset it emits is ESC ( B -- three bytes. Removing only the printable `(B` leaves a bare ESC
# sitting between `bench` and `:`, so the `bench:` below stops matching and this script returns
# ZERO rows for a run whose benches all completed. That is the dangerous shape: a log with 36
# results parses as "no benches ran". Seen for real on 2026-08-27 (sysb0827c) against 2026-08-26
# logs that were uncoloured and parsed fine.
# Matches ONLY the userspace sysbench rows, which are Rust bench format:
#   test benches::object_create_delete ... bench:  114,107.95 ns/iter (+/- 25,147.95)
# The kernel test suite in the same log also prints "ns/iter" (as "Average:/Min:/Max:") and an
# earlier version of this matched those too, emitting nameless rows of plausible-looking numbers.
sed 's/\x1b\[[0-9;]*[a-zA-Z]//g; s/\x1b([A-Z]//g' "$1" | awk -v c="$2" -v r="$3" '
/^[[:space:]]*test .* bench:.*ns\/iter/ {
  name=$2; sub(/^benches::/,"",name);
  mean=""; spread="";
  for (i=1;i<=NF;i++) {
    if ($i=="ns/iter") { mean=$(i-1) }
    if ($i=="(+/-")    { spread=$(i+1); sub(/\)$/,"",spread) }
  }
  gsub(/,/,"",mean); gsub(/,/,"",spread);
  if (name!="" && mean!="") print c"\t"r"\t"name"\t"mean"\t"spread
}'
