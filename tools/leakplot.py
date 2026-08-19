#!/usr/bin/env python3
"""Turn a leakcheck serial log into graphs and a ranked summary.

Reads the LEAKCHECK-* lines out of a boot log (or stdin), refits every counter series in the same
way the in-guest code does, and plots one grid per operation with the fit overlaid.

The fit is deliberately duplicated rather than trusted from the log: the guest's numbers are the
verdict of record, and a disagreement between the two is worth seeing.

    tools/leakplot.py boot.log -o out/
    tools/leakplot.py boot.log --summary-only
    many.py --autostart leakcheck ... && tools/leakplot.py <log> -o leakout/
"""

import argparse
import math
import os
import re
import sys
from collections import defaultdict

# Slope must explain the series this well before it is called a leak rather than churn.
R2_MIN = 0.9
# No single step may account for more than this much of the tail's growth. This, not r2, is what
# separates a leak from background work: a counter that climbs in two jumps of four fits a line
# about as well as one that climbs by one every iteration. Both thresholds match the guest-side
# ones in main.rs::verdict -- change them together.
MAX_STEP_FRAC = 0.34


def parse(lines):
    counters = {}        # index -> (name, kind)
    samples = defaultdict(list)   # op -> [ (iter, [values]) ]
    ops = {}             # op -> metadata dict
    guest_fits = defaultdict(dict)   # op -> name -> dict
    guest_leaks = defaultdict(list)  # op -> [name]
    header = {}

    for ln in lines:
        if "LEAKCHECK" not in ln:
            continue
        # Serial logs interleave; find the token and parse from there.
        i = ln.index("LEAKCHECK")
        f = ln[i:].split()
        tag = f[0]

        if tag == "LEAKCHECK-BEGIN":
            header = dict(kv.split("=", 1) for kv in f[1:] if "=" in kv)
        elif tag == "LEAKCHECK-COUNTER" and len(f) >= 4:
            counters[int(f[1])] = (f[2], f[3])
        elif tag == "LEAKCHECK-OP" and len(f) >= 2:
            ops[f[1]] = dict(kv.split("=", 1) for kv in f[2:] if "=" in kv)
        elif tag == "LEAKCHECK-SAMPLE" and len(f) >= 3:
            op, it = f[1], int(f[2])
            try:
                vals = [int(x) for x in f[3:]]
            except ValueError:
                continue
            samples[op].append((it, vals))
        elif tag == "LEAKCHECK-FIT" and len(f) >= 4:
            op, name = f[1], f[2]
            d = dict(kv.split("=", 1) for kv in f[3:] if "=" in kv)
            d["kind"] = f[3] if "=" not in f[3] else "?"
            guest_fits[op][name] = d
        elif tag == "LEAKCHECK-LEAK" and len(f) >= 3:
            guest_leaks[f[1]].append(f[2])

    for op in samples:
        samples[op].sort(key=lambda t: t[0])
    return header, counters, samples, ops, guest_fits, guest_leaks


def fit(ys):
    """Least squares of y against index, plus the shape statistics.

    Returns dict(slope, r2, growth, duty, max_step_frac) or None. Mirrors fit.rs.
    """
    n = len(ys)
    if n < 3:
        return None
    xbar = (n - 1) / 2.0
    ybar = sum(ys) / n
    sxy = sxx = syy = 0.0
    for i, y in enumerate(ys):
        dx, dy = i - xbar, y - ybar
        sxy += dx * dy
        sxx += dx * dx
        syy += dy * dy
    if sxx == 0:
        return None
    slope = sxy / sxx
    r2 = 1.0 if syy == 0 else (sxy * sxy) / (sxx * syy)
    growth = ys[-1] - ys[0]
    rises = [b - a for a, b in zip(ys, ys[1:]) if b > a]
    return dict(slope=slope, r2=r2, growth=growth,
                duty=len(rises) / (n - 1),
                max_step_frac=(max(rises) / growth) if (rises and growth > 0) else 0.0)


def analyse(counters, samples, warmup):
    """op -> list of per-counter result dicts, tail-fitted."""
    out = {}
    for op, rows in samples.items():
        series = [v for _, v in rows]
        if not series:
            continue
        start = min(warmup, max(0, len(series) - 3))
        res = []
        for ci in sorted(counters):
            name, kind = counters[ci]
            full = [s[ci] for s in series if ci < len(s)]
            if len(full) != len(series):
                continue
            # u64::MAX is the absent sentinel -- a failed gate call, not a value.
            if any(v == (1 << 64) - 1 for v in full):
                res.append(dict(name=name, kind=kind, absent=True))
                continue
            f = fit(full[start:])
            if f is None:
                continue
            res.append(dict(name=name, kind=kind, absent=False, series=full, start=start, **f))
        out[op] = res
    return out


def is_leak(r):
    return (not r.get("absent") and r["kind"] == "level" and r["slope"] > 0
            and r["r2"] >= R2_MIN and r["growth"] >= 1.0
            and r["max_step_frac"] <= MAX_STEP_FRAC)


def summary(results, ops, guest_leaks):
    for op in sorted(results):
        meta = ops.get(op, {})
        conv = ""
        if meta:
            conv = (f"  [pre {meta.get('pre_converged','?')}/{meta.get('pre_ms','?')}ms"
                    f" post {meta.get('post_converged','?')}/{meta.get('post_ms','?')}ms]")
        print(f"\n=== {op}{conv}")
        if meta.get("post_converged") == "false":
            print("    !! post-quiesce did not converge: something is still moving. "
                  "Time-to-settle is itself a finding.")

        rows = [r for r in results[op] if not r.get("absent")]
        leaks = [r for r in rows if is_leak(r)]
        absent = [r["name"] for r in results[op] if r.get("absent")]

        if leaks:
            print(f"    {'counter':<24} {'slope/iter':>12} {'r2':>6} {'growth':>10} "
                  f"{'duty':>6} {'maxstep':>8}")
            for r in sorted(leaks, key=lambda r: -r["slope"]):
                mark = "" if r["name"] in guest_leaks.get(op, []) else "   (guest disagreed)"
                print(f"    {r['name']:<24} {r['slope']:>12.4f} {r['r2']:>6.3f} "
                      f"{r['growth']:>10.1f} {r['duty']:>6.2f} {r['max_step_frac']:>8.2f}{mark}")
        else:
            print("    no level counter shows a sustained positive slope")

        # Near-misses are worth a line: a steep slope that failed only on r2 is usually churn, but
        # a slope that failed only on growth is a leak too small for this N to resolve.
        near = [r for r in rows if r["kind"] == "level" and r["slope"] > 0 and not is_leak(r)
                and (r["r2"] >= 0.5 or r["growth"] >= 1.0)]
        if near:
            def why(r):
                if r["max_step_frac"] > MAX_STEP_FRAC:
                    return "stepped"      # background work, not per-iteration accrual
                if r["r2"] < R2_MIN:
                    return "noisy"
                return "too-small"        # a real gradient, below what this N can resolve
            print("    near misses: " + ", ".join(
                f"{r['name']}({why(r)} slope={r['slope']:.3f} r2={r['r2']:.2f} "
                f"growth={r['growth']:.0f} maxstep={r['max_step_frac']:.2f})"
                for r in sorted(near, key=lambda r: -r["slope"])[:6]))
        if absent:
            print("    absent (gate call failed): " + ", ".join(absent))


def check_controls(results):
    """The two arms that decide whether anything else in the report means anything."""
    print("\n=== controls")
    ok = True

    null = next((op for op in results if op.startswith("l0")), None)
    if null is None:
        print("    !! no null control ran. Every other number is unvalidated.")
        ok = False
    else:
        bad = [r for r in results[null] if is_leak(r)]
        if bad:
            ok = False
            print(f"    !! null control {null} shows slope on: "
                  + ", ".join(f"{r['name']}({r['slope']:.3f})" for r in bad))
            print("       The instrument is measuring itself. Fix this before reading anything else.")
        else:
            print(f"    null control {null}: clean")

    pos = next((op for op in results if op.startswith("p1")), None)
    if pos is None:
        print("    !! no positive control ran. A clean report has not been shown to be capable "
              "of being dirty.")
        ok = False
    else:
        detected = [r for r in results[pos] if is_leak(r)]
        if not detected:
            ok = False
            print(f"    !! positive control {pos} was NOT detected. The harness cannot see a "
                  "leak it is deliberately creating; every 'clean' below is meaningless.")
        else:
            obj = next((r for r in detected if r["name"] == "obj.objects"), None)
            if obj:
                print(f"    positive control {pos}: detected, obj.objects "
                      f"{obj['slope']:.3f}/iter (expected ~1.0)")
            else:
                print(f"    positive control {pos}: detected on "
                      + ", ".join(r["name"] for r in detected[:4]))
    return ok


def plot(results, outdir, ops):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    os.makedirs(outdir, exist_ok=True)
    written = []
    for op in sorted(results):
        rows = [r for r in results[op] if not r.get("absent") and "series" in r]
        # Constant series carry no information and would fill the grid with flat lines; keep them
        # only if nothing else moved, so an all-clean op still renders something.
        moving = [r for r in rows if len(set(r["series"])) > 1]
        show = moving if moving else rows[:6]
        if not show:
            continue

        ncol = 4
        nrow = math.ceil(len(show) / ncol)
        fig, axes = plt.subplots(nrow, ncol, figsize=(4 * ncol, 2.6 * nrow), squeeze=False)
        for ax in axes.flat:
            ax.set_visible(False)

        for k, r in enumerate(sorted(show, key=lambda r: (-int(is_leak(r)), r["name"]))):
            ax = axes[k // ncol][k % ncol]
            ax.set_visible(True)
            ys = r["series"]
            xs = list(range(len(ys)))
            leak = is_leak(r)
            ax.plot(xs, ys, lw=1.0, color="#c0392b" if leak else "#2c3e50")

            s = r["start"]
            if s > 0:
                ax.axvspan(0, s - 0.5, color="0.9", zorder=0)
            # The fitted line, over the tail only -- the region it was actually fitted to.
            tail = ys[s:]
            if len(tail) >= 3:
                tb = sum(tail) / len(tail)
                xb = (len(tail) - 1) / 2.0
                ax.plot([s + i for i in range(len(tail))],
                        [tb + r["slope"] * (i - xb) for i in range(len(tail))],
                        ls="--", lw=1.2, color="#e67e22")

            title = f"{r['name']}  {r['slope']:+.3f}/it"
            if leak:
                title += "  LEAK"
            ax.set_title(title, fontsize=8,
                         color="#c0392b" if leak else "black")
            ax.tick_params(labelsize=6)
            ax.margins(x=0.02)

        meta = ops.get(op, {})
        fig.suptitle(f"leakcheck: {op}   (grey = warmup, discarded; dashed = tail fit)"
                     + (f"   post-quiesce converged={meta.get('post_converged','?')}"
                        if meta else ""), fontsize=10)
        fig.tight_layout(rect=(0, 0, 1, 0.97))
        path = os.path.join(outdir, f"{op}.png")
        fig.savefig(path, dpi=110)
        plt.close(fig)
        written.append(path)
    return written


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log", nargs="?", default="-", help="boot/serial log, or - for stdin")
    ap.add_argument("-o", "--outdir", default=None, help="write PNGs here")
    ap.add_argument("--warmup", type=int, default=None,
                    help="override the warmup the guest used")
    ap.add_argument("--summary-only", action="store_true")
    args = ap.parse_args()

    src = sys.stdin if args.log == "-" else open(args.log, errors="replace")
    header, counters, samples, ops, guest_fits, guest_leaks = parse(src)

    if not counters:
        print("no LEAKCHECK-COUNTER lines found -- was this a leakcheck boot?", file=sys.stderr)
        return 2
    if not samples:
        print("counters but no samples: leakcheck ran with --no-samples, or the boot died "
              "mid-run. Only the guest's own fits are available.", file=sys.stderr)
        return 2

    warmup = args.warmup if args.warmup is not None else int(header.get("warmup", 10))
    results = analyse(counters, samples, warmup)

    # An op that was asked for and produced nothing is not an op that read clean. Name it, because
    # silence and success look identical in a table of rows that only lists what was found -- which
    # is exactly how a build-failed arm of an A/B read as "no data, carry on" three times tonight.
    skipped = {op for op in ops if op not in set(results)}
    if skipped:
        print("!! op(s) the guest announced but produced no samples for: "
              + ", ".join(sorted(skipped)))

    print(f"leakcheck: {len(results)} ops, {len(counters)} counters, warmup={warmup}")
    controls_ok = check_controls(results)
    summary(results, ops, guest_leaks)

    if not controls_ok:
        print("\n!! controls failed -- treat every result above as unvalidated.")

    if args.outdir and not args.summary_only:
        written = plot(results, args.outdir, ops)
        print(f"\nwrote {len(written)} plots to {args.outdir}/")
    return 0


if __name__ == "__main__":
    sys.exit(main())
