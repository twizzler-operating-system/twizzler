#!/usr/bin/env python3
"""Wait for an artifact, never for a process pattern.

Every session on this box has independently rediscovered that `pgrep -f <pattern>` matches the
wrapper shell carrying the pattern in its own command line — a launcher that waits on it hangs
forever, a liveness check reports a dead sweep alive. The rule "verify by artifact, not by
process pattern" is written down in at least three places and has failed to prevent a recurrence
in any of them, because at the moment of writing a check, pgrep is the convenient thing. This
helper exists to be more convenient than pgrep.

Usage:
    ./waitfor.py PATH_OR_GLOB [--grep MARKER] [--timeout SECS] [--interval SECS]

Exits 0 when a file matching PATH_OR_GLOB exists (and, with --grep, contains MARKER); exits 2 on
timeout. Prints what it was waiting for and what satisfied it, so a transcript shows the evidence
rather than an inference.

Examples:
    # wait for a sweep to produce its first round log
    ./waitfor.py 'target/results/many-mytag/round1-*.log' --timeout 900
    # wait for a sweep driver to finish (many.py writes this line last)
    ./waitfor.py scratch/mytag-driver.log --grep 'summary written' --timeout 7200
"""

import argparse
import glob
import sys
import time


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("pattern", help="path or glob of the artifact to wait for")
    p.add_argument("--grep", help="also require this substring in the (first matching) file")
    p.add_argument("--timeout", type=float, default=3600.0)
    p.add_argument("--interval", type=float, default=5.0)
    args = p.parse_args()

    deadline = time.monotonic() + args.timeout
    while True:
        for path in sorted(glob.glob(args.pattern)):
            if args.grep is None:
                print(f"waitfor: {path} exists")
                return 0
            try:
                with open(path, "r", errors="replace") as f:
                    if args.grep in f.read():
                        print(f"waitfor: {path} contains {args.grep!r}")
                        return 0
            except OSError:
                pass
        if time.monotonic() >= deadline:
            print(
                f"waitfor: TIMEOUT after {args.timeout:.0f}s waiting for {args.pattern!r}"
                + (f" containing {args.grep!r}" if args.grep else ""),
                file=sys.stderr,
            )
            return 2
        time.sleep(args.interval)


if __name__ == "__main__":
    sys.exit(main())
