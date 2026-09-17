# synctest.sh -- does `ctl sync-all` find a live dirty mapping?
#
#   brush synctest.sh [MB]
#
# `should_sync` is set on a *mapping*, by the runtime's file-write path, and it only exists while
# the process that wrote the file still has it open. Once that process exits, the unmap hands the
# object to the kernel's background sync thread instead -- and on an idle machine that thread
# usually gets there first, so a sweep run afterwards reports 0 not because there was nothing to
# sync but because there is nothing *left*. Every earlier `sync-all` test hit exactly that.
#
# So the file is written three ways, and the interesting number is how the three differ:
#
#   1. held open by this shell across the sync   -- deterministic, no race with anything
#   2. a writer still running during the sync    -- the same thing without relying on `exec`
#   3. everything closed and gone                -- the baseline that has always read 0
#
# Kept to POSIX constructs, as with ctlsweep.sh.

set -u

MB=${1:-8}
BIG=$((MB * 32))
# /ext is the pager-backed external namespace, which is the point -- these writes have to reach a
# real backing store. Overridable so the script's own logic can be exercised on a host.
DIR=${SYNCTEST_DIR:-/ext}
F1=$DIR/synctest-held.bin
F2=$DIR/synctest-live.bin
F3=$DIR/synctest-closed.bin

blocks() { echo $(($1 * 1024)); }
report() {
    # `wc -c` rather than `ls -l`: it is the byte count that says whether the data landed.
    printf '   %s: ' "$1"
    wc -c < "$1" 2>/dev/null || echo "(missing)"
}

rm -f "$F1" "$F2" "$F3"

echo "== phase 1: $MB MB written by this shell itself, to an fd it keeps open"
# `printf` in a loop, not `dd`: the flag is set by whichever process *writes*, and a child does
# not do. An earlier version wrote with `dd ... >&3` and read 0 here -- dd set `should_sync` in
# its own context and took it away again when it exited, which is phase 3 wearing a disguise.
exec 3> "$F1"
i=0
n=$((MB * 256))
while [ "$i" -lt "$n" ]; do
    printf '%4096s' '' >&3
    i=$((i + 1))
done
echo "== ctl sync-all (this shell holds the file open and wrote it)"
ctl sync-all
exec 3>&-
report "$F1"

echo "== phase 2: $BIG MB being written by a background job during the sync"
# No `$!` here: brush expanded it to nothing last run. `wait` with no arguments waits for every
# background job, which is all this needs.
dd if=/dev/zero of="$F2" bs=1024 count="$(blocks "$BIG")" 2>/dev/null &
sleep 1
echo "== ctl sync-all (writer should still be running)"
ctl sync-all
wait
report "$F2"

echo "== phase 3: $MB MB written and closed before the sync"
dd if=/dev/zero of="$F3" bs=1024 count="$(blocks "$MB")" 2>/dev/null
report "$F3"
echo "== ctl sync-all (nothing open)"
ctl sync-all

echo "== files after all syncs"
report "$F1"
report "$F2"
report "$F3"
echo "== synctest done"
