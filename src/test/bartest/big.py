#!/usr/bin/env python3
"""Run `bartest big` on an ivshmem BAR backed by a host file, then check the file.

Run from the repo root; extra arguments go to `cargo start-qemu`. The file is sparse and only the
probe pages are ever touched by this script.
"""

import argparse
import os
import struct
import subprocess
import sys

GIB = 1 << 30


def probes(size):
    for c in range(0, size, GIB):
        yield from (c, c + GIB // 2 + 8, c + GIB - 4096, c + GIB - 8)


def host_marker(off):
    return 0x4854 << 48 | off


def guest_marker(off):
    return 0x4753 << 48 | off


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--file", default="target/bartest-big.mem")
    p.add_argument("--size-gib", type=int, default=4)
    p.add_argument("qemu_args", nargs="*")
    a = p.parse_args()
    size = a.size_gib * GIB

    with open(a.file, "wb") as f:
        f.truncate(size)
        for off in probes(size):
            f.seek(off)
            f.write(struct.pack("<Q", host_marker(off)))

    cmd = [
        "cargo", "start-qemu", "--ssh-port", "0", "--autostart", "bartest big",
        "-q=--nographic",
        "-q=-object", f"-q=memory-backend-file,id=big,size={size},mem-path={a.file},share=on",
        "-q=-device", "-q=ivshmem-plain,memdev=big",
    ] + a.qemu_args
    out = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=1800)
    log = out.stdout.decode(errors="replace")
    sys.stdout.write(log)
    passed = "bartest: PASS" in log

    bad = []
    with open(a.file, "rb") as f:
        for off in probes(size):
            f.seek(off)
            (v,) = struct.unpack("<Q", f.read(8))
            if v != guest_marker(off):
                bad.append(f"{off:#x}: {v:#x}")
    print(f"big.py: guest {'PASS' if passed else 'FAIL'} (exit {out.returncode})")
    for b in bad:
        print(f"big.py: host file at {b}, expected the guest marker")
    print(f"big.py: host file {'PASS' if not bad else 'FAIL'}")
    sys.exit(0 if passed and not bad else 1)


if __name__ == "__main__":
    main()
