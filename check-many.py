#!/usr/bin/env python3
"""Report on every `many.py` sweep currently running on this machine.

Sweeps are quiet by design -- status goes to the console that launched them and to their own
driver.log -- so once one is running in a background shell there is otherwise no way to ask how far
along it is. This reads that state back out of /proc and each sweep's directory.

A sweep is located by the driver.log it holds open, not by its arguments, so a sweep whose tag was
auto-generated is found just as easily as one given `--tag`.
"""

import argparse
import contextlib
import fcntl
import io
import os
import re
import shutil
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Set, Tuple

REPO_ROOT = Path(__file__).resolve().parent
CLOCK_TICKS = os.sysconf("SC_CLK_TCK")
RESULTS_ROOT = REPO_ROOT / "target" / "results"
WORK_ROOT = REPO_ROOT / "target" / "many-work"

# The sweep's own schedule, so an ETA knows what is still pending. Imported rather than
# reimplemented: which configurations a sweep runs is many.py's decision, and a stale copy of it
# here would be wrong in exactly the cases an ETA matters.
sys.path.insert(0, str(REPO_ROOT))
try:
    import many
except ImportError:  # still useful without an ETA
    many = None

# How long to sample cpu counters over. Long enough to be steady, short enough that a status check
# still feels instant.
CPU_SAMPLE_SECONDS = 0.4

TOTAL_RE = re.compile(r"^tag (\S+): running (\d+) configurations across (\d+) lanes")
START_RE = re.compile(r"^\[lane (\d+)\] start\s+(\S+)")
DONE_RE = re.compile(r"^\[lane (\d+)\] (PASS|FAIL)\s+(\S+)\s+([\d.]+)m\s+(.*)$")
BUILD_RE = re.compile(r"^=== building (\S+)")
FINAL_RE = re.compile(r"^(\d+) passed, (\d+) failed$")


@dataclass
class Sweep:
    pid: int
    elapsed: float
    results: Optional[Path] = None
    tag: Optional[str] = None
    total: Optional[int] = None
    lanes: Optional[int] = None
    phase: str = "starting"
    passed: int = 0
    failed: int = 0
    done: List[Tuple[str, str, str, str]] = field(default_factory=list)
    running: Dict[int, str] = field(default_factory=dict)


def proc_cmdline(pid: int) -> List[str]:
    try:
        raw = Path(f"/proc/{pid}/cmdline").read_bytes()
    except OSError:
        return []
    return [a for a in raw.decode("utf-8", "replace").split("\0") if a]


def proc_elapsed(pid: int) -> float:
    """Seconds since the process started, from its start time against system uptime."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        uptime = float(Path("/proc/uptime").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return 0.0
    # comm can contain spaces and parens, so fields are counted from after the last ')'.
    fields = stat[stat.rfind(")") + 2 :].split()
    try:
        return max(0.0, uptime - float(fields[19]) / CLOCK_TICKS)
    except (IndexError, ValueError):
        return 0.0


def proc_cpu_seconds(pid: int) -> Optional[float]:
    """Cpu time the process has burned so far, user + system."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    fields = stat[stat.rfind(")") + 2 :].split()
    try:
        return (float(fields[11]) + float(fields[12])) / CLOCK_TICKS
    except (IndexError, ValueError):
        return None


def sample_cpu(pids: List[int]) -> Dict[int, float]:
    """Percent of one cpu each pid is using right now, sampled over a short window.

    A guest's share of the machine is what says whether a lane is working or starved, and the
    counter is cumulative -- averaged over the whole run it would just converge and stop being
    informative. All pids are read either side of one shared sleep so a status check costs one
    sample window regardless of how many lanes are up.
    """
    before = {pid: proc_cpu_seconds(pid) for pid in pids}
    if not any(v is not None for v in before.values()):
        return {}
    time.sleep(CPU_SAMPLE_SECONDS)
    now: Dict[int, float] = {}
    for pid in pids:
        start, end = before.get(pid), proc_cpu_seconds(pid)
        if start is not None and end is not None:
            now[pid] = max(0.0, (end - start) / CPU_SAMPLE_SECONDS * 100.0)
    return now


def system_load() -> str:
    """Machine-wide load, for reading a lane's cpu share against what the host has left."""
    try:
        one = float(Path("/proc/loadavg").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return ""
    return f"load {one:.1f}/{os.cpu_count() or '?'}"


def find_sweeps() -> List[int]:
    pids = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        argv = proc_cmdline(int(entry.name))
        # Basename, not endswith: this script is itself called check-many.py.
        if any(os.path.basename(a) == "many.py" for a in argv):
            pids.append(int(entry.name))
    return sorted(pids)


def results_dir_of(pid: int) -> Optional[Path]:
    """Locate a sweep's directory via the driver.log it is holding open."""
    try:
        for fd in Path(f"/proc/{pid}/fd").iterdir():
            target = os.readlink(fd)
            if target.endswith("/driver.log"):
                return Path(target).parent
    except OSError:
        pass
    # Not open yet (or not readable): fall back to what the command line asked for.
    argv = proc_cmdline(pid)
    for flag, build in (("--results-dir", lambda v: Path(v)),
                        ("--tag", lambda v: REPO_ROOT / "target" / "results" / f"many-{v}")):
        if flag in argv:
            i = argv.index(flag)
            if i + 1 < len(argv):
                return build(argv[i + 1])
    return None


def read_status(sweep: Sweep) -> None:
    log = (sweep.results or Path("/nonexistent")) / "driver.log"
    try:
        lines = log.read_text(errors="replace").splitlines()
    except OSError:
        return

    building: Optional[str] = None
    for line in lines:
        if m := TOTAL_RE.match(line):
            sweep.tag, sweep.total, sweep.lanes = m.group(1), int(m.group(2)), int(m.group(3))
            building = None
        elif m := BUILD_RE.match(line):
            building = m.group(1)
        elif m := START_RE.match(line):
            sweep.running[int(m.group(1))] = m.group(2)
            building = None
        elif m := DONE_RE.match(line):
            lane, verdict, name, mins, summary = m.groups()
            sweep.running.pop(int(lane), None)
            sweep.done.append((verdict, name, mins, summary))
            if verdict == "PASS":
                sweep.passed += 1
            else:
                sweep.failed += 1
        elif m := FINAL_RE.match(line):
            sweep.phase = "finishing"

    if sweep.phase != "finishing":
        sweep.phase = f"building {building}" if building else "running"


@dataclass
class LaneProc:
    """The qemu behind one in-flight lane."""

    pid: int
    mode: str
    smp: str
    elapsed: float
    cpu: Optional[float] = None

    @property
    def desc(self) -> str:
        return f"qemu {self.pid} {self.mode} smp{self.smp}"

    @property
    def load(self) -> str:
        return f"cpu {self.cpu:.0f}%" if self.cpu is not None else ""


def lane_qemus(tag: Optional[str]) -> Dict[int, LaneProc]:
    """Map lane index to its qemu: what it is, how long it has been up, what it is using.

    The driver log carries no timestamps, so the guest process is what actually knows how long the
    run in that lane has been going.
    """
    found: Dict[int, LaneProc] = {}
    if not tag:
        return found
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        argv = proc_cmdline(pid)
        if not argv or "qemu-system" not in argv[0]:
            continue
        joined = " ".join(argv)
        m = re.search(rf"/lanes/{re.escape(tag)}/lane(\d+)/", joined)
        if not m:
            continue
        found[int(m.group(1))] = LaneProc(
            pid=pid,
            mode="KVM" if "-enable-kvm" in argv else "TCG",
            smp=argv[argv.index("-smp") + 1] if "-smp" in argv else "?",
            elapsed=proc_elapsed(pid),
        )
    cpus = sample_cpu([p.pid for p in found.values()])
    for proc in found.values():
        proc.cpu = cpus.get(proc.pid)
    return found


# --- estimating -----------------------------------------------------------------------------


def config_of(run: str) -> str:
    """`round3-debug-nokvm-smp1` names the run; `debug-nokvm-smp1` is what predicts its duration."""
    return run.split("-", 1)[1] if "-" in run else run


def sweep_schedule(pid: int) -> Optional[List[str]]:
    """Every run this sweep will work through, rebuilt from its command line."""
    if many is None:
        return None
    argv = proc_cmdline(pid)
    rest: Optional[List[str]] = None
    for i, arg in enumerate(argv):
        if os.path.basename(arg) == "many.py":
            rest = argv[i + 1 :]
            break
    if rest is None:
        return None
    try:
        # argparse writes usage to stderr and exits on anything it dislikes; a schedule we cannot
        # rebuild costs an ETA, and must not cost the status report.
        with contextlib.redirect_stderr(io.StringIO()):
            args = many.build_parser().parse_args(rest)
        return [
            f"round{n}-{c.name}"
            for n, c in many.build_jobs(args.rounds, args.slow_rounds, args.enable_slow_debug)
        ]
    except SystemExit:
        return None
    except Exception:
        return None


SUMMARY_RE = re.compile(r"^round \d+\s+(\S+)\s+(?:PASS|FAIL)\s+([\d.]+)m")


def historical_durations() -> Dict[str, List[float]]:
    """Per-configuration run times recorded by previous sweeps.

    Round 1 is when an ETA is most useful and when a sweep knows least about itself, and the spread
    between configurations is two orders of magnitude (a release+KVM run is under a minute, a
    debug+TCG one an hour) -- so an average over "whatever has finished so far" is not a usable
    stand-in for a configuration with no sample yet. Old numbers from the same machine are.

    Failed runs count too, deliberately: the question is when the sweep ends, and a configuration
    that reliably panics three minutes in does take three minutes. It does make the estimate for a
    flaky configuration jumpy, since a panic and a hang are nothing alike in length.
    """
    out: Dict[str, List[float]] = {}
    if not RESULTS_ROOT.is_dir():
        return out
    for entry in RESULTS_ROOT.iterdir():
        try:
            text = (entry / "summary.txt").read_text(errors="replace")
        except OSError:
            continue
        for line in text.splitlines():
            if m := SUMMARY_RE.match(line):
                out.setdefault(m.group(1), []).append(float(m.group(2)))
    return out


def estimates(sweep: "Sweep", history: Dict[str, List[float]]) -> Dict[str, float]:
    """Minutes a run of each configuration takes: this sweep's own numbers first, history second."""
    est = {name: sum(v) / len(v) for name, v in history.items()}
    own: Dict[str, List[float]] = {}
    for _verdict, name, mins, _summary in sweep.done:
        own.setdefault(config_of(name), []).append(float(mins))
    est.update({name: sum(v) / len(v) for name, v in own.items()})
    return est


def eta_minutes(
    sweep: "Sweep", schedule: Optional[List[str]], est: Dict[str, float],
    qemus: Dict[int, LaneProc],
) -> Optional[float]:
    """Roughly how much longer the sweep has, in minutes.

    Lane-time rather than wall-clock extrapolation: the driver log has no timestamps, so how long
    the sweep has already been running says nothing about how far through the work it is. Summing
    what is left and dividing by the lanes needs neither.

    It is a floor, not a promise -- --max-slow can leave lanes idle rather than start another
    debug+TCG run, and the final job of a sweep runs alone however many lanes are free.
    """
    if not sweep.lanes:
        return None

    remaining = 0.0
    # A run already past its estimate is not a finished one; keep a floor under it so a stuck lane
    # cannot pull the whole estimate to zero.
    tails = []
    for lane, name in sweep.running.items():
        ran = qemus[lane].elapsed / 60 if lane in qemus else 0.0
        left = max(est.get(config_of(name), 0.0) - ran, 1.0)
        remaining += left
        tails.append(left)

    if schedule is not None:
        done = {name for _v, name, _m, _s in sweep.done}
        pending = [j for j in schedule if j not in done and j not in sweep.running.values()]
        if not est and pending:
            return None
        fallback = sum(est.values()) / len(est) if est else 0.0
        remaining += sum(est.get(config_of(j), fallback) for j in pending)
        tails.extend(est.get(config_of(j), fallback) for j in pending)
    elif sweep.total is not None and sweep.done:
        # No schedule to work from: fall back to this sweep's own average pace, which is only
        # meaningful once a mix of configurations has finished.
        avg = sum(float(m) for _v, _n, m, _s in sweep.done) / len(sweep.done)
        left = max(0, sweep.total - len(sweep.done) - len(sweep.running))
        remaining += avg * left
        tails.append(avg if left else 0.0)
    elif not sweep.running:
        return None

    # However well the work packs, nothing finishes before its longest single remaining run does.
    return max(remaining / sweep.lanes, max(tails, default=0.0))


def human_time(minutes: float) -> str:
    return f"{minutes:.0f}m" if minutes < 90 else f"{minutes / 60:.1f}h"


def activity(results: Optional[Path], name: str) -> str:
    """Size of the in-flight run's transcript and how long since the guest last wrote to it."""
    if not results:
        return ""
    path = results / f"{name}.log"
    try:
        st = path.stat()
    except OSError:
        return "no output yet"
    quiet = time.time() - st.st_mtime
    return f"{st.st_size / 1024:.0f}KB, quiet {quiet:.0f}s"


def rel(path: Path) -> str:
    try:
        return str(path.resolve().relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def human(size: int) -> str:
    """Results directories are megabytes and lane images are gigabytes; one unit suits neither."""
    return f"{size / 2**30:.1f}GB" if size >= 2**30 else f"{size / 2**20:.0f}MB"


def disk_bytes(path: Path) -> int:
    """Blocks actually allocated, not apparent size -- these images are 100GB of mostly hole."""
    total = 0
    for root, _dirs, files in os.walk(path):
        for name in files:
            try:
                total += os.lstat(os.path.join(root, name)).st_blocks * 512
            except OSError:
                pass
    return total


def newest_mtime(path: Path) -> float:
    newest = 0.0
    for root, _dirs, files in os.walk(path):
        for name in files:
            try:
                newest = max(newest, os.lstat(os.path.join(root, name)).st_mtime)
            except OSError:
                pass
    return newest or path.stat().st_mtime


def lane_is_dead(tag_dir: Path, live_tags: Set[str]) -> bool:
    """A lane directory belongs to a dead sweep if we can take its owner lock.

    Sweeps hold that lock for their whole run. Directories predating the lock fall back to the set
    of tags we can see running, which is why that set is passed in rather than guessed at here.
    """
    if tag_dir.name in live_tags:
        return False
    owner = tag_dir / ".owner"
    if not owner.exists():
        return True
    try:
        with owner.open("r+") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except (OSError, BlockingIOError):
        return False


def cleanup(sweeps: List[Sweep], args: argparse.Namespace) -> int:
    """Remove finished sweeps' results and dead sweeps' lane images."""
    live_dirs = {s.results.resolve() for s in sweeps if s.results}
    live_tags = {s.tag for s in sweeps if s.tag}
    cutoff = time.time() - args.older_than * 3600
    victims: List[Tuple[Path, int, float]] = []

    # Only many-* directories: target/results also holds hand-named result sets that are not ours
    # to delete.
    if RESULTS_ROOT.is_dir():
        for entry in sorted(RESULTS_ROOT.iterdir()):
            if not entry.is_dir() or not entry.name.startswith("many-"):
                continue
            if entry.resolve() in live_dirs:
                continue
            age = newest_mtime(entry)
            if age <= cutoff:
                victims.append((entry, disk_bytes(entry), age))

    lanes = WORK_ROOT / "lanes"
    if lanes.is_dir():
        for entry in sorted(lanes.iterdir()):
            if entry.is_dir() and lane_is_dead(entry, live_tags):
                victims.append((entry, disk_bytes(entry), newest_mtime(entry)))

    if args.include_images:
        images = WORK_ROOT / "images"
        if images.is_dir() and not sweeps:
            victims.append((images, disk_bytes(images), newest_mtime(images)))
        elif images.is_dir():
            print("keeping master images: a sweep is running and may be copying from them")

    if not victims:
        print("nothing to clean up")
        return 0

    total = sum(size for _, size, _ in victims)
    print(f"would remove {len(victims)} item(s), reclaiming {human(total)}:")
    for path, size, age in victims:
        hours = (time.time() - age) / 3600
        print(f"    {human(size):>8}  {hours:5.1f}h old  {rel(path)}")

    if not args.yes:
        if not sys.stdin.isatty():
            print("\nnot a terminal; re-run with --yes to delete", file=sys.stderr)
            return 1
        if input("\ndelete these? [y/N] ").strip().lower() not in ("y", "yes"):
            print("left alone")
            return 0

    removed = 0
    for path, _size, _age in victims:
        try:
            shutil.rmtree(path)
            removed += 1
        except OSError as e:
            print(f"failed to remove {rel(path)}: {e}", file=sys.stderr)
    print(f"removed {removed} item(s), reclaimed about {human(total)}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cleanup", action="store_true",
                        help="Delete results and lane images belonging to sweeps that are no "
                             "longer running. Lists them and asks before removing anything.")
    parser.add_argument("--older-than", type=float, default=0.0, metavar="HOURS",
                        help="With --cleanup, only remove results untouched for this many hours "
                             "(default: 0, any finished sweep). Lane images from dead sweeps are "
                             "always removable -- nothing can be using them.")
    parser.add_argument("--include-images", action="store_true",
                        help="With --cleanup, also drop the shared master snapshots in "
                             "many-work/images. They cost a full rebuild to regenerate, so they "
                             "are kept by default, and always kept while a sweep is running.")
    parser.add_argument("--yes", action="store_true",
                        help="Skip the confirmation prompt.")
    args = parser.parse_args()

    pids = find_sweeps()
    if not pids and not args.cleanup:
        print("no many.py sweeps running")
        return 0
    if not pids:
        print("no many.py sweeps running")

    history = historical_durations()

    sweeps: List[Sweep] = []
    for n, pid in enumerate(pids):
        if n:
            print()
        sweep = Sweep(pid=pid, elapsed=proc_elapsed(pid), results=results_dir_of(pid))
        read_status(sweep)
        sweeps.append(sweep)

        tag = sweep.tag or "?"
        load = system_load()
        print(f"many.py  pid {pid}  tag {tag}  elapsed {sweep.elapsed / 60:.1f}m  [{sweep.phase}]"
              + (f"  {load}" if load else ""))
        if sweep.results:
            print(f"  results  {rel(sweep.results)}")

        qemus = lane_qemus(sweep.tag)
        est = estimates(sweep, history)
        eta = eta_minutes(sweep, sweep_schedule(pid), est, qemus)

        if sweep.total:
            complete = sweep.passed + sweep.failed
            print(f"  progress {complete}/{sweep.total} done"
                  f"  ({sweep.passed} passed, {sweep.failed} failed)"
                  f"  {len(sweep.running)} in flight"
                  + (f"  eta ~{human_time(eta)}" if eta else ""))

        for lane in sorted(sweep.running):
            name = sweep.running[lane]
            proc = qemus.get(lane)
            detail = proc.desc if proc else "copying images"
            ran = proc.elapsed / 60 if proc else 0.0
            # Elapsed against this configuration's usual run time: the two are only meaningful
            # together, so they share a column rather than becoming two.
            expect = est.get(config_of(name))
            age = (f"{ran:.1f}m" if ran else "--") + (f"/~{expect:.0f}m" if expect else "")
            print(f"    lane {lane}  {name:<24} {age:>12}  {detail:<22} "
                  f"{(proc.load if proc else ''):>8}  {activity(sweep.results, name)}")

        for verdict, name, mins, summary in [d for d in sweep.done if d[0] == "FAIL"]:
            print(f"    FAILED   {name:<24} {mins}m  {summary}")

    if args.cleanup:
        if sweeps:
            print()
        return cleanup(sweeps, args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
