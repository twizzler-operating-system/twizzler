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
DONE_RE = re.compile(r"^\[lane (\d+)\] (PASS|FAIL)\s+(\S+)\s+([\dhms.]+)\s+(.*)$")
BUILD_RE = re.compile(r"^=== building (\S+)")
FINAL_RE = re.compile(r"^(\d+) passed, (\d+) failed$")

HMS_RE = re.compile(r"^(?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s)?$")
MINUTES_RE = re.compile(r"^([\d.]+)m$")


def parse_duration(text: str) -> Optional[float]:
    """Seconds from many.py's `42s`/`5m03s`/`1h05m`, or from the bare `12.3m` older sweeps wrote.

    Both forms are accepted because the estimates lean on summary.txt files already on disk, which
    predate the second-resolution format.
    """
    if (m := HMS_RE.match(text)) and any(m.groups()):
        h, mins, secs = (int(g or 0) for g in m.groups())
        return h * 3600.0 + mins * 60.0 + secs
    if m := MINUTES_RE.match(text):
        return float(m.group(1)) * 60.0
    return None


def human_time(seconds: float) -> str:
    """`42s` / `5m03s` / `1h05m` -- the same shape many.py logs, so the two read alike."""
    total = max(0, round(seconds))
    if total < 60:
        return f"{total}s"
    if total < 3600:
        return f"{total // 60}m{total % 60:02d}s"
    return f"{total // 3600}h{total % 3600 // 60:02d}m"


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
    done: List[Tuple[str, str, float, str]] = field(default_factory=list)
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


def proc_ppid(pid: int) -> Optional[int]:
    """Parent pid, from /proc. qemu's parent is the xtask that spawned it."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    # Same counting rule as proc_elapsed: comm can contain spaces and parens.
    fields = stat[stat.rfind(")") + 2 :].split()
    try:
        return int(fields[1])
    except (IndexError, ValueError):
        return None


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
            lane, verdict, name, took, summary = m.groups()
            sweep.running.pop(int(lane), None)
            sweep.done.append((verdict, name, parse_duration(took) or 0.0, summary))
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


def lane_qemus(tag: Optional[str]) -> Dict[str, LaneProc]:
    """Map run name (`round1-release-kvm-smp4`) to its qemu: what it is, how long it has been up.

    The driver log carries no timestamps, so the guest process is what actually knows how long the
    run in that lane has been going.

    Keyed by run name rather than lane index because there is nothing lane-specific left in a qemu
    command line: lanes stopped copying images and now share one set of masters, so the `lane<N>/`
    path segment this used to match on is gone. Matching it kept returning nothing, which the
    caller read as "no qemu yet, so it must still be copying" -- for the whole run. The run name
    comes from the `--label` on qemu's parent xtask, which many.py sets to `<tag>-<run>`.
    """
    found: Dict[str, LaneProc] = {}
    if not tag:
        return found
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        argv = proc_cmdline(pid)
        if not argv or "qemu-system" not in argv[0]:
            continue
        ppid = proc_ppid(pid)
        parent = proc_cmdline(ppid) if ppid else []
        if "--label" not in parent or parent.index("--label") + 1 >= len(parent):
            continue
        label = parent[parent.index("--label") + 1]
        if not label.startswith(f"{tag}-"):
            continue
        found[label[len(tag) + 1 :]] = LaneProc(
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


def quadrant_of(config: str) -> str:
    """`debug-nokvm-smp1` -> `debug-nokvm`: the profile/accel pair that sets a run's cost tier."""
    return config.rsplit("-smp", 1)[0]


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
        # The whole namespace, so a new selection flag over there cannot silently drop out of the
        # schedule here.
        return [f"round{n}-{c.name}" for n, c in many.build_jobs(args)]
    except SystemExit:
        return None
    except Exception:
        return None


SUMMARY_RE = re.compile(r"^round \d+\s+(\S+)\s+(PASS|FAIL)\s+([\dhms.]+)")


def historical_durations() -> Dict[str, List[Tuple[float, bool]]]:
    """Per-configuration `(seconds, passed)` recorded by previous sweeps.

    Round 1 is when an ETA is most useful and when a sweep knows least about itself, and the spread
    between configurations is two orders of magnitude (a release+KVM run is under a minute, a
    debug+TCG one an hour) -- so an average over "whatever has finished so far" is not a usable
    stand-in for a configuration with no sample yet. Old numbers from the same machine are.

    Whether the run passed is kept, because a failure is a *lower bound* on a healthy run, not a
    sample of one -- see `pick_estimate`.
    """
    out: Dict[str, List[Tuple[float, bool]]] = {}
    if not RESULTS_ROOT.is_dir():
        return out
    for entry in RESULTS_ROOT.iterdir():
        try:
            text = (entry / "summary.txt").read_text(errors="replace")
        except OSError:
            continue
        for line in text.splitlines():
            if m := SUMMARY_RE.match(line):
                if (secs := parse_duration(m.group(3))) is not None:
                    out.setdefault(m.group(1), []).append((secs, m.group(2) == "PASS"))
    return out


def pick_estimate(samples: List[Tuple[float, bool]]) -> float:
    """How long a run of this configuration should be expected to take.

    Not the mean, and passes outrank failures. Both corrections exist because the sweeps this
    history comes from are the ones that were finding crashes, and the sweeps it is used on are the
    ones checking the crashes are gone:

    - A crash is a lower bound. `debug-nokvm-smp2`'s whole history was two runs that panicked at 3.0m
      and 4.0m, giving a 3.5m estimate for a configuration whose healthy run takes an hour -- and
      `debug-nokvm-smp1`, which had one passing run, was estimated correctly at 60m. So prefer
      passing samples whenever any exist.
    - A capped run (heartbeat/silence timeout) is censored, not measured; it says "at least this".

    Taking the max of whatever survives keeps the failure direction safe: an ETA that reads high
    wastes patience, one that reads low says a sweep is nearly done when it has an hour left.
    """
    passes = [s for s, ok in samples if ok]
    return max(passes) if passes else max(s for s, _ in samples)


def estimates(sweep: "Sweep", history: Dict[str, List[Tuple[float, bool]]]) -> Dict[str, float]:
    """Seconds a run of each configuration takes: this sweep's own numbers first, history second."""
    own: Dict[str, List[Tuple[float, bool]]] = {}
    for verdict, name, secs, _summary in sweep.done:
        own.setdefault(config_of(name), []).append((secs, verdict == "PASS"))
    # This sweep's own numbers replace history rather than joining it: they are the only ones taken
    # from this tree, at this concurrency.
    effective = {name: v for name, v in history.items() if v}
    effective.update({name: v for name, v in own.items() if v})
    est = {name: pick_estimate(v) for name, v in effective.items()}

    # A configuration with no passing sample anywhere still has siblings. Cost is set by profile and
    # accel -- the tiering many.py schedules by -- far more than by cpu count, so borrow the
    # quadrant's slowest healthy run rather than trusting a crash time. Without this,
    # `debug-nokvm-smp2` reads 4m off two panics while `debug-nokvm-smp1` correctly reads 60m.
    healthy: Dict[str, float] = {}
    for name, v in effective.items():
        if any(ok for _secs, ok in v):
            healthy[quadrant_of(name)] = max(healthy.get(quadrant_of(name), 0.0), est[name])
    for name, v in effective.items():
        if not any(ok for _secs, ok in v):
            est[name] = max(est[name], healthy.get(quadrant_of(name), 0.0))
    return est


def eta_seconds(
    sweep: "Sweep", schedule: Optional[List[str]], est: Dict[str, float],
    qemus: Dict[str, LaneProc],
) -> Optional[float]:
    """Roughly how much longer the sweep has, in seconds.

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
    for _lane, name in sweep.running.items():
        ran = qemus[name].elapsed if name in qemus else 0.0
        left = max(est.get(config_of(name), 0.0) - ran, 60.0)
        remaining += left
        tails.append(left)

    if schedule is not None:
        done = {name for _v, name, _d, _s in sweep.done}
        pending = [j for j in schedule if j not in done and j not in sweep.running.values()]
        if not est and pending:
            return None
        fallback = sum(est.values()) / len(est) if est else 0.0
        remaining += sum(est.get(config_of(j), fallback) for j in pending)
        tails.extend(est.get(config_of(j), fallback) for j in pending)
    elif sweep.total is not None and sweep.done:
        # No schedule to work from: fall back to this sweep's own average pace, which is only
        # meaningful once a mix of configurations has finished.
        avg = sum(secs for _v, _n, secs, _s in sweep.done) / len(sweep.done)
        left = max(0, sweep.total - len(sweep.done) - len(sweep.running))
        remaining += avg * left
        tails.append(avg if left else 0.0)
    elif not sweep.running:
        return None

    # However well the work packs, nothing finishes before its longest single remaining run does.
    return max(remaining / sweep.lanes, max(tails, default=0.0))


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
            print("keeping master images: a sweep is running and may be booting from them")

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
        print(f"many.py  pid {pid}  tag {tag}  elapsed {human_time(sweep.elapsed)}  [{sweep.phase}]"
              + (f"  {load}" if load else ""))
        if sweep.results:
            print(f"  results  {rel(sweep.results)}")

        qemus = lane_qemus(sweep.tag)
        est = estimates(sweep, history)
        eta = eta_seconds(sweep, sweep_schedule(pid), est, qemus)

        if sweep.total:
            complete = sweep.passed + sweep.failed
            print(f"  progress {complete}/{sweep.total} done"
                  f"  ({sweep.passed} passed, {sweep.failed} failed)"
                  f"  {len(sweep.running)} in flight"
                  + (f"  eta ~{human_time(eta)}" if eta else ""))

        for lane in sorted(sweep.running):
            name = sweep.running[lane]
            proc = qemus.get(name)
            # No qemu for a started run means it is between "[lane N] start" and qemu coming up --
            # xtask's own startup. There is no image-copy phase to be in any more.
            detail = proc.desc if proc else "starting"
            ran = proc.elapsed if proc else 0.0
            # Elapsed against this configuration's usual run time: the two are only meaningful
            # together, so they share a column rather than becoming two.
            expect = est.get(config_of(name))
            age = (human_time(ran) if ran else "--") + (f"/~{human_time(expect)}" if expect else "")
            print(f"    lane {lane}  {name:<24} {age:>12}  {detail:<22} "
                  f"{(proc.load if proc else ''):>8}  {activity(sweep.results, name)}")

        for verdict, name, secs, summary in [d for d in sweep.done if d[0] == "FAIL"]:
            print(f"    FAILED   {name:<24} {human_time(secs):>7}  {summary}")

    if args.cleanup:
        if sweeps:
            print()
        return cleanup(sweeps, args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
