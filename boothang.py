#!/usr/bin/env python3
"""Boot-only reproducer for the boot-before-tests hang.

The hang sits between the PIT statclock print and `[kernel::machine::pcie] init`, i.e. entirely
inside boot, so a full 67s test round is ~20x more wall clock than the question needs. This boots
the sweep's own master image, waits for the pcie marker, and on a miss dumps guest state through a
dedicated monitor socket -- the evidence that cracked the analogous B1 serial-ISR wedge, and the
one thing the timed-out sweep rounds never captured.

Faithful to many.py/xtask's qemu invocation except for two deliberate changes:
  * `-serial file:` + `-monitor unix:` instead of `-serial mon:stdio`, so the monitor is reachable
    on a wedged guest without fighting the stdio muxer for the escape character.
  * `-display none` in place of `--nographic` (which would re-bind serial to stdio).
Both are outside the guest; the guest sees the same machine, cpu flags, memory and devices.
"""
import argparse, os, socket, subprocess, sys, threading, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SUCCESS = b"[kernel::machine::pcie] init"
# Printed immediately before the window the hang lives in; its absence means we never got close.
PRIOR = b"setting up for statclock"


def qemu_cmd(boot_img, data_img, serial_log, mon_sock, smp, port, bios):
    return [
        "qemu-system-x86_64",
        "-m", "12000,slots=4,maxmem=128G",
        "-bios", str(bios),
        "-machine", "q35,nvdimm=on",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-enable-kvm",
        "-cpu", "host,+x2apic,+tsc-deadline,+invtsc,+tsc,+rdtscp",
        "-drive", f"format=raw,file={boot_img},snapshot=on",
        "-drive", f"file={data_img},if=none,id=nvme,snapshot=on",
        "-device", "nvme,serial=deadbeef,drive=nvme",
        "-device", "virtio-net-pci,netdev=net0",
        "-netdev", f"user,id=net0,hostfwd=tcp::{port}-:5555",
        "--no-reboot",
        "-serial", f"file:{serial_log}",
        "-monitor", f"unix:{mon_sock},server,nowait",
        "-vga", "virtio",
        "-smp", str(smp),
        "-display", "none",
    ]


def _hmp(s, cmd, settle=3.0):
    s.sendall((cmd + "\n").encode())
    deadline = time.time() + settle
    buf = b""
    while time.time() < deadline:
        try:
            chunk = s.recv(65536)
        except socket.timeout:
            break
        if not chunk:
            break
        buf += chunk
        if buf.rstrip().endswith(b"(qemu)"):
            break
    return buf.decode("utf8", "replace")


RSP_RE = None


def stack_dump(s, regs_text, words=512):
    """Dump each cpu's stack.

    The monitor reports only the innermost RIP. When a cpu is spinning inside an interrupt handler,
    the frame that matters -- what it was doing when the interrupt landed -- is on the stack below,
    and is the difference between inferring a re-entrancy and observing it.
    """
    import re
    out = []
    cpus = re.findall(r"^CPU#(\d+)", regs_text, re.M)
    rsps = re.findall(r"RSP=([0-9a-f]+)", regs_text)
    for cpu, rsp in zip(cpus, rsps):
        out.append(f"\n===== cpu {cpu} stack @ 0x{rsp} =====\n")
        out.append(_hmp(s, f"cpu {cpu}"))
        out.append(_hmp(s, f"x/{words}gx 0x{rsp}", settle=5.0))
    return "".join(out)


def monitor_dump(sock_path, commands, timeout=10.0):
    """Talk HMP over the monitor socket. Returns the transcript, or an error string."""
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(str(sock_path))
    except OSError as e:
        return f"<monitor connect failed: {e}>"
    out = []
    try:
        time.sleep(0.3)
        try:
            out.append(s.recv(65536).decode("utf8", "replace"))
        except socket.timeout:
            pass
        for cmd in commands:
            if cmd.startswith("!sleep "):
                time.sleep(float(cmd.split()[1]))
                out.append(f"\n===== slept {cmd.split()[1]}s =====\n")
                continue
            s.sendall((cmd + "\n").encode())
            deadline = time.time() + 3.0
            buf = b""
            while time.time() < deadline:
                try:
                    chunk = s.recv(65536)
                except socket.timeout:
                    break
                if not chunk:
                    break
                buf += chunk
                if buf.rstrip().endswith(b"(qemu)"):
                    break
            out.append(f"\n===== {cmd} =====\n" + buf.decode("utf8", "replace"))
        regs_text = "".join(t for t in out if "CPU#" in t)
        if regs_text:
            out.append(stack_dump(s, regs_text))
    except OSError as e:
        out.append(f"<monitor io error: {e}>")
    finally:
        s.close()
    return "".join(out)


# Two register samples around a sleep: one snapshot cannot tell a cpu spinning on a lock (RIP
# pinned) from one livelocking across a loop body or still making progress (RIP moves). That
# distinction is the whole question here, so it is built into the capture rather than inferred.
DUMP_CMDS = [
    "info cpus",
    "info registers -a",
    "info lapic",
    "info irq",
    "!sleep 2.0",
    "info registers -a",
    "!sleep 2.0",
    "info registers -a",
]


def one_round(n, args, boot_img, data_img, bios, results, lock):
    tagdir = Path(args.outdir)
    serial_log = tagdir / f"round{n:05d}.log"
    mon_sock = tagdir / f"round{n:05d}.mon"
    for p in (serial_log, mon_sock):
        if p.exists():
            p.unlink()
    port = args.port_base + (n % 4000)
    cmd = qemu_cmd(boot_img, data_img, serial_log, mon_sock, args.smp, port, bios)
    t0 = time.time()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    verdict, elapsed = "UNKNOWN", 0.0
    try:
        while True:
            elapsed = time.time() - t0
            if proc.poll() is not None:
                data = serial_log.read_bytes() if serial_log.exists() else b""
                verdict = "PASS_EXIT" if SUCCESS in data else "EARLY_EXIT"
                break
            data = serial_log.read_bytes() if serial_log.exists() else b""
            if SUCCESS in data:
                verdict = "PASS"
                break
            if elapsed > args.timeout:
                # The whole point of the run: capture guest state while it is still wedged.
                dump = monitor_dump(mon_sock, DUMP_CMDS)
                (tagdir / f"round{n:05d}.dump").write_text(dump)
                verdict = "HANG" if PRIOR in data else "HANG_EARLY"
                break
            time.sleep(0.05)
    finally:
        if proc.poll() is None:
            proc.kill()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
        if mon_sock.exists():
            mon_sock.unlink()
    # Keep only interesting transcripts; a clean boot log is 51 identical lines x thousands of runs.
    if verdict.startswith("PASS") and not args.keep_all:
        serial_log.unlink(missing_ok=True)
    with lock:
        results.append((n, verdict, elapsed))
        print(f"round{n:05d} {verdict:10s} {elapsed:6.1f}s", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=100)
    ap.add_argument("--jobs", type=int, default=3)
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--timeout", type=float, default=30.0)
    ap.add_argument("--lane", default="boothang", help="many.py tag whose masters to boot")
    ap.add_argument("--profile", default="debug")
    ap.add_argument("--outdir", default=str(ROOT / "boothang-work" / "run1"))
    ap.add_argument("--port-base", type=int, default=52000)
    # Explicit images beat the lane lookup: many.py's prune_dead_lanes rmtree's any tag whose
    # .owner it can flock, and a tag whose sweep has exited qualifies -- so masters left under
    # target/many-work/lanes/ can vanish mid-arm when anyone launches anything.
    ap.add_argument("--boot-img", default=None)
    ap.add_argument("--data-img", default=None)
    ap.add_argument("--keep-all", action="store_true")
    args = ap.parse_args()

    masters = ROOT / "target" / "many-work" / "lanes" / args.lane / "masters"
    boot_img = Path(args.boot_img) if args.boot_img else masters / f"{args.profile}-boot.img"
    data_img = Path(args.data_img) if args.data_img else masters / f"{args.profile}-data.img"
    bios = sorted(ROOT.glob("toolchain/toolchain_*/OVMF.fd"))
    if not boot_img.exists() or not data_img.exists():
        sys.exit(f"missing master images under {masters}")
    if not bios:
        sys.exit("no OVMF.fd found")
    Path(args.outdir).mkdir(parents=True, exist_ok=True)

    results, lock = [], threading.Lock()
    sem = threading.Semaphore(args.jobs)
    threads = []

    def worker(n):
        try:
            one_round(n, args, boot_img, data_img, bios[0], results, lock)
        finally:
            sem.release()

    t0 = time.time()
    for n in range(1, args.rounds + 1):
        sem.acquire()
        t = threading.Thread(target=worker, args=(n,), daemon=True)
        t.start()
        threads.append(t)
    for t in threads:
        t.join()

    counts = {}
    for _, v, _ in results:
        counts[v] = counts.get(v, 0) + 1
    print(f"\n=== {len(results)} rounds in {time.time()-t0:.0f}s ===")
    for k in sorted(counts):
        print(f"{counts[k]:6d}  {k}")
    Path(args.outdir, "SUMMARY").write_text(
        f"rounds={len(results)} wall={time.time()-t0:.0f}s\n"
        + "".join(f"{counts[k]} {k}\n" for k in sorted(counts))
        + "".join(f"round{n:05d} {v} {e:.1f}\n" for n, v, e in sorted(results))
    )
    Path(args.outdir, ".done").write_text("done\n")


if __name__ == "__main__":
    main()
