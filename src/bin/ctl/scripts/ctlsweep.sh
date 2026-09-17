# ctlsweep.sh -- run a workload with the kernel's maintenance commands around it.
#
#   brush ctlsweep.sh [-d CLASSES] [-z] [-x CODE] [--] [workload [args...]]
#
# Exists because `--autostart` runs exactly one program, so covering anything that needs a
# workload *and* a `ctl` call has meant one boot per command. This is that one program: it drives
# the workload and the maintenance around it in a single boot, and reports each step.
#
#   -d CLASSES  arm kernel diagnostics before the workload (comma-separated, or `all`)
#   -z          sweep every free frame instead of a single zeroing pass; slow on a big machine
#   -x CODE     power the machine off with CODE when finished, instead of returning
#   --          stop option parsing; everything after is the workload
#
# Exit code is the workload's if it ran and failed, otherwise the first failing step, otherwise 0.
# Kept to POSIX constructs -- no arrays, no `local`, no `getopts` -- so it does not depend on
# which bash extensions the brush port has.

set -u

diag=""
zero_all=0
exit_code=""
failed=0

usage() {
    echo "usage: brush ctlsweep.sh [-d CLASSES] [-z] [-x CODE] [--] [workload [args...]]" >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        -d) [ $# -ge 2 ] || usage; diag="$2"; shift 2 ;;
        -z) zero_all=1; shift ;;
        -x) [ $# -ge 2 ] || usage; exit_code="$2"; shift 2 ;;
        --) shift; break ;;
        -*) echo "ctlsweep: unknown option $1" >&2; usage ;;
        *)  break ;;
    esac
done

# Run one step, announce it, and remember the first failure without stopping: a sync that fails
# should not cost you the reap and the dump, which are the things that say why.
step() {
    echo "== $*"
    "$@"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "== FAILED ($rc): $*"
        if [ "$failed" -eq 0 ]; then
            failed="$rc"
        fi
    fi
    return "$rc"
}

if [ -n "$diag" ]; then
    step ctl diag --on "$diag"
fi

# The workload's own exit code outranks the maintenance steps': it is what the run was about.
work_rc=0
if [ $# -gt 0 ]; then
    echo "== workload: $*"
    "$@"
    work_rc=$?
    echo "== workload exited $work_rc"
fi

# Dump before the maintenance runs, so it shows the state the workload left rather than the state
# after everything has been reclaimed.
step ctl debug-dump

# Sync before reap: reaping deletes objects, and a delete makes their dirty pages moot.
step ctl sync-all
# Reap before zero: reaped objects return frames, which zeroing can then clean.
step ctl reap-all
if [ "$zero_all" -eq 1 ]; then
    step ctl zero-all
else
    step ctl zero-all --no-wait
fi

if [ "$work_rc" -ne 0 ]; then
    status="$work_rc"
else
    status="$failed"
fi

if [ -n "$exit_code" ]; then
    echo "== powering off with code $exit_code"
    # Does not return.
    ctl shutdown "$exit_code"
fi

echo "== ctlsweep done (status $status)"
exit "$status"
