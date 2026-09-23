#!/usr/bin/env python3
"""Turn a qemu-monitor dump of a wedged guest into a per-cpu verdict and a backtrace.

xtask asks the monitor for every vcpu's registers and stack when it declares a guest dead (see
`dump_guest_state` in tools/xtask/src/qemu.rs), because that is the one diagnostic needing no
cooperation from the guest: it works when a cpu spins with interrupts masked, when the console lock
is held, and when everything is halted waiting on a wakeup that never came. Those three are exactly
what a silent hang cannot otherwise tell apart, and the dump separates them:

    every cpu HLT=1              -- nobody is running. A lost wakeup, not a deadlock.
    a cpu with HLT=0 and CPL=0   -- spinning in the kernel; the backtrace names where.
    a cpu with HLT=0 and CPL=3   -- the wedge is in userspace, not the kernel path you suspect.

RIP alone frequently is not enough, since a spinning cpu's RIP sits in a spin helper that does not
say who called it. So every stack word that looks like kernel text is symbolized too: some are live
return addresses and some are dead slots left from earlier calls, which is why this is a *candidate*
backtrace and not a real unwind. Read it as "the kernel was recently in these functions, innermost
first" -- in practice that is enough to name the caller, which is the thing RIP omits.

Usage:
    ./symbolize.py target/results/.../round1-release-nokvm-smp2-FAILED.log [--profile release]
    ./symbolize.py <log> --frames 20     # show more candidate frames per cpu
"""

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import List, Optional, Tuple

REPO_ROOT = Path(__file__).resolve().parent

CPU_RE = re.compile(r"CPU#(\d+)\b")
# The monitor puts RIP, CPL and HLT on one line, which is everything the verdict needs.
RIP_RE = re.compile(r"RIP=([0-9a-fA-F]+).*?CPL=(\d).*?HLT=(\d)")
RSP_RE = re.compile(r"RSP=([0-9a-fA-F]+)")
# `x/64gx $rsp` output: an address, a colon, then the words held there.
MEM_RE = re.compile(r"([0-9a-fA-F]{8,16}):((?:\s+0x[0-9a-fA-F]+)+)\s*$")

# Anything below this is not kernel text; on x86_64 the kernel lives in the higher half.
KERNEL_MIN = 0xFFFF_8000_0000_0000
WORD = 8


def kernel_elf(profile: str) -> Path:
    return REPO_ROOT / "target" / "kernel" / "x86_64-unknown-none" / profile / "twizzler-kernel"


def addr2line_cmd() -> Optional[List[str]]:
    """llvm-addr2line first: GNU addr2line mis-parses this kernel's DWARF.

    On a debug build it reports `DWARF error: mangled line number section` and then answers anyway,
    with line numbers that cannot be trusted. Function attribution happened to agree when checked
    against the symbol table, but a tool that warns and continues is not one to prefer.
    """
    for cmd in ("llvm-addr2line", "addr2line"):
        if shutil.which(cmd):
            return [cmd]
    return None


def sections(elf: Path) -> List[Tuple[str, int, int, bool]]:
    """(name, start, end, executable) for allocated sections, so an address outside .text can say
    where it actually landed. A kernel RIP in .data is a wild jump, and reporting it as "no symbol"
    buries the single most important thing in the dump."""
    try:
        out = subprocess.run(
            ["readelf", "-SW", str(elf)], capture_output=True, text=True, check=True
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError):
        return []
    found = []
    for line in out.splitlines():
        m = re.search(
            r"\]\s+(\S+)\s+\S+\s+([0-9a-f]{8,16})\s+[0-9a-f]+\s+([0-9a-f]+)\s+\S*\s*([A-Z]*)", line
        )
        if not m:
            continue
        start, size = int(m.group(2), 16), int(m.group(3), 16)
        if start and size and "A" in m.group(4):
            found.append((m.group(1), start, start + size, "X" in m.group(4)))
    return found


def locate(addr: int, secs: List[Tuple[str, int, int, bool]]) -> Optional[Tuple[str, bool]]:
    for name, start, end, execable in secs:
        if start <= addr < end:
            return name, execable
    return None


def symbolize(elf: Path, addrs):
    """addr -> "func at file:line", resolved in one addr2line call."""
    addrs = sorted(set(addrs))
    if not addrs:
        return {}
    tool = addr2line_cmd()
    if tool is None:
        print("warning: no addr2line found; reporting raw addresses", file=sys.stderr)
        return {}
    try:
        out = subprocess.run(
            tool + ["-fpCe", str(elf), *[f"0x{a:x}" for a in addrs]],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.splitlines()
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        print(f"warning: addr2line failed ({e}); reporting raw addresses", file=sys.stderr)
        return {}
    return {a: line.strip() for a, line in zip(addrs, out)}


def stack_blocks(lines):
    """Contiguous runs of `x` output, as {start_address: [words]}.

    Blocks are keyed by where they start so a cpu can claim its own by RSP. That matters because
    xtask asks about more cpus than a run may have, and qemu answers a `cpu N` it does not have by
    leaving the selection alone -- which would otherwise attribute a repeat of the previous cpu's
    stack to the missing one.
    """
    rows = []
    for line in lines:
        m = MEM_RE.search(line)
        if m:
            words = [int(w, 16) for w in m.group(2).split()]
            rows.append((int(m.group(1), 16), words))

    blocks, start, acc, expect = {}, None, [], None
    for addr, words in rows:
        if start is None or addr != expect:
            if start is not None:
                blocks.setdefault(start, acc)
            start, acc = addr, []
        acc.extend(words)
        expect = addr + WORD * len(words)
    if start is not None:
        blocks.setdefault(start, acc)
    return blocks


def parse(text):
    """The last dump in the file, as ([cpu records], {start: stack words}).

    The last one, because a transcript may hold more than one and the final is the one taken at
    death. Everything from the closing `CPU#0` block onward belongs to it.
    """
    lines = text.splitlines()
    starts = [i for i, l in enumerate(lines) if CPU_RE.search(l) and CPU_RE.search(l).group(1) == "0"]
    if not starts:
        return [], {}
    lines = lines[starts[-1] :]

    cpus, cur = [], None
    for line in lines:
        m = CPU_RE.search(line)
        if m:
            cur = {"cpu": int(m.group(1)), "rip": None, "cpl": None, "halted": None, "rsp": None}
            cpus.append(cur)
        if cur is None:
            continue
        m = RIP_RE.search(line)
        if m:
            cur["rip"] = int(m.group(1), 16)
            cur["cpl"] = int(m.group(2))
            cur["halted"] = m.group(3) == "1"
        m = RSP_RE.search(line)
        if m:
            cur["rsp"] = int(m.group(1), 16)

    # Drop repeats of a cpu index and any block that never got a RIP line.
    seen, uniq = set(), []
    for c in cpus:
        if c["rip"] is not None and c["cpu"] not in seen:
            seen.add(c["cpu"])
            uniq.append(c)
    return sorted(uniq, key=lambda c: c["cpu"]), stack_blocks(lines)


def candidate_frames(words, limit, secs):
    """Words that could be return addresses, innermost first, without consecutive repeats.

    "Could be" means inside an executable section -- a stack holds plenty of higher-half words that
    are data pointers (statics, heap, other stacks), and admitting those on the strength of the
    address range alone fills the backtrace with confident-looking nonsense. A word repeated back to
    back is almost always one return address saved twice rather than real recursion.
    """
    out = []
    for w in words:
        here = locate(w, secs)
        if here and here[1] and (not out or out[-1] != w):
            out.append(w)
        if len(out) >= limit:
            break
    return out


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("log", type=Path, help="serial transcript containing the monitor dump")
    ap.add_argument("--profile", default="release", help="which kernel build to symbolize against")
    ap.add_argument("--frames", type=int, default=10, help="candidate frames per cpu (default: 10)")
    args = ap.parse_args()

    cpus, blocks = parse(args.log.read_text(errors="replace"))
    if not cpus:
        print(f"no `info registers -a` dump found in {args.log}", file=sys.stderr)
        print("(the guest exited on its own, or died before xtask could ask the monitor)", file=sys.stderr)
        return 1

    elf = kernel_elf(args.profile)
    if not elf.exists():
        print(f"warning: no kernel at {elf}; reporting raw addresses", file=sys.stderr)
    secs = sections(elf) if elf.exists() else []

    for c in cpus:
        c["frames"] = candidate_frames(blocks.get(c["rsp"], []), args.frames, secs)

    if elf.exists():
        wanted = [c["rip"] for c in cpus if c["rip"] >= KERNEL_MIN]
        wanted += [f for c in cpus for f in c["frames"]]
        syms = symbolize(elf, wanted)
    else:
        syms = {}

    def name(addr, is_rip=False):
        sym = syms.get(addr, "")
        where = locate(addr, secs)
        # A RIP outside every executable section is the whole finding, not a footnote: the cpu is
        # executing something that is not code. Only worth saying about a RIP -- a stack word in
        # .data is just an ordinary data pointer.
        tag = ""
        if is_rip and where and not where[1]:
            tag = f"[!! executing {where[0]}, NOT an executable section -- wild jump !!] "
        elif is_rip and where is None and addr >= KERNEL_MIN:
            tag = "[!! outside every kernel section !!] "
        if sym and not sym.startswith("??"):
            return tag + sym
        if where:
            return tag + f"<no symbol, in {where[0]}>"
        return tag + ("<not kernel text>" if addr < KERNEL_MIN else "<unmapped>")

    for c in cpus:
        state = "halted" if c["halted"] else f"running CPL={c['cpl']}"
        print(f"cpu{c['cpu']}  {state:<14} rip={c['rip']:#018x}  {name(c['rip'], is_rip=True)}")
        if c["frames"]:
            print("       stack (candidates, innermost first):")
            for f in c["frames"]:
                print(f"         {f:#018x}  {name(f)}")
        elif c["rsp"] is not None and c["rsp"] not in blocks:
            print("       stack: unavailable (qemu could not read it)")
        else:
            print("       stack: read, but holds no kernel text (userspace or firmware stack)")
        print()

    print("(a halted cpu's rip is wherever it executed hlt, normally the idle loop. Stack entries")
    print(" are candidates, not a real unwind: some are dead slots from earlier calls.)")

    running = [c for c in cpus if not c["halted"]]
    in_kernel = [c for c in running if c["cpl"] == 0 and c["rip"] >= KERNEL_MIN]
    in_user = [c for c in running if c["cpl"] == 3]
    elsewhere = [c for c in running if c["cpl"] == 0 and c["rip"] < KERNEL_MIN]

    def names(cs):
        return ", ".join(f"cpu{c['cpu']}" for c in cs)

    print()
    if not running:
        print("VERDICT: every cpu halted -- nothing is running, so this is a lost wakeup,")
        print("         not a deadlock. Look at who should have posted the wakeup.")
    elif in_kernel:
        print(f"VERDICT: {names(in_kernel)} running in the kernel -- a spin or livelock there.")
        if in_user:
            print(f"         ({names(in_user)} also running, in userspace.)")
    elif in_user:
        print(f"VERDICT: only userspace running ({names(in_user)}) -- the wedge is above the")
        print("         kernel. For the pager handshake that means pager-srv, not the Ready arm.")
    else:
        print(f"VERDICT: {names(elsewhere)} running outside kernel text (firmware or early boot).")
        print("         Probably a dump taken before the guest reached the kernel.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
