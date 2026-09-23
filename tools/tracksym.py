#!/usr/bin/env python3
"""Symbolize KALLOC-TRACK-SITE lines from a leakcheck boot log.

The kernel records raw return addresses because symbolizing from inside GlobalAlloc::alloc
allocates (DWARF) and deadlocks. Resolution happens here instead.

Usage: tracksym.py <boot.log> <kernel-elf>

The kernel ELF must be the one the image actually booted -- the path `make-image` prints as
`kernel: "..."`, i.e. deps/twizzler_kernel-<hash>, not target/kernel/<triple>/<profile>/
twizzler-kernel, which drifts. Verify by resolving one address the guest itself named.
"""
import re, subprocess, sys, os

A2L = os.environ.get("ADDR2LINE", "llvm-addr2line-18")

def sym(elf, addrs):
    if not addrs:
        return {}
    p = subprocess.run([A2L, "-f", "-C", "-i", "-e", elf] + ["%#x" % a for a in addrs],
                       capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit("%s failed: %s" % (A2L, p.stderr.strip()))
    lines = [l for l in p.stdout.splitlines()]
    # -i can emit several (func, file) pairs per address; without a separator we cannot split them
    # reliably, so re-run one address at a time when the count does not match.
    if len(lines) != 2 * len(addrs):
        out = {}
        for a in addrs:
            q = subprocess.run([A2L, "-f", "-C", "-e", elf, "%#x" % a],
                               capture_output=True, text=True)
            ls = q.stdout.splitlines()
            out[a] = (ls[0] if ls else "??", ls[1] if len(ls) > 1 else "??")
        return out
    return {a: (lines[2 * i], lines[2 * i + 1]) for i, a in enumerate(addrs)}

def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    log, elf = sys.argv[1], sys.argv[2]
    text = open(log, errors="replace").read()

    # SITE and the BLOCK line that follows it belong together.
    sites = []
    pat = re.compile(r"KALLOC-TRACK-SITE count=(\d+) size=(\d+) oldest=(\d+) newest=(\d+) ips=([0-9a-fx,]+)"
                     r"(?:.*?\n.*?KALLOC-TRACK-BLOCK ptr=(\S+) size=\d+ bytes=(\S+))?")
    for m in pat.finditer(text):
        count, size = int(m.group(1)), int(m.group(2))
        oldest, newest = int(m.group(3)), int(m.group(4))
        ips = [int(x, 16) for x in m.group(5).split(",") if x not in ("0", "0x0")]
        sites.append((count, size, ips, m.group(6), m.group(7), oldest, newest))
    if not sites:
        print("no KALLOC-TRACK-SITE lines in %s" % log)
    for m in re.finditer(r"KALLOC-TRACK-TOTAL .*", text):
        print(m.group(0))
    for m in re.finditer(r"LEAKCHECK-TRACK .*", text):
        print(m.group(0))

    allad = sorted({a for s in sites for a in s[2]})
    table = sym(elf, allad)
    sites.sort(key=lambda s: -s[0])
    for count, size, ips, ptr, raw, oldest, newest in sites:
        print("\n=== %d live block(s), size %d, seq %d..%d ===" % (count, size, oldest, newest))
        for i, a in enumerate(ips):
            f, l = table.get(a, ("??", "??"))
            print("  %d  %#018x  %s\n         %s" % (i, a, f, l))
        if raw:
            words = [raw[i:i+16] for i in range(0, len(raw), 16)]
            le = ["0x" + "".join(reversed([w[j:j+2] for j in range(0, len(w), 2)])) for w in words]
            print("  example %s  %s" % (ptr, " ".join(le)))

main()
