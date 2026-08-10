#!/usr/bin/env python3
"""Run the Twizzler test suite across build/accel/SMP configurations, concurrently.

Each run boots a test image in qemu and watches the serial console for the guest's test report.

Runs used to be serialized because they all shared two pieces of global state: the boot image in
the build tree and `target/disk-<triple>.img`, the ext4 disk qemu attaches as nvme + virtio-pmem.
Serializing never actually protected them -- nothing stopped a developer's own `start-qemu` from
taking the disk's write lock or port 5555 mid-sweep, and when that happened the run died with
`Failed to get "write" lock` and got recorded as a mysterious exit 34.

So instead: build each profile once, snapshot its boot image and ext4 disk into a work directory,
and give every lane private copies to run against (`xtask test --boot-image/--disk-image`). Nothing
in the sweep touches the build tree after the build phase, which both lets lanes run concurrently
-- across different profiles, not just different qemu flags -- and leaves the development tree free
to build and boot while a sweep is in flight.

Lanes reuse their copies across runs and re-sync only when they switch profile, which preserves the
existing semantics that guest-written disk state carries from one run to the next.

The same reasoning applies one level up: several sweeps can run at once, and alongside an ordinary
`cargo xtask test`, because everything a sweep writes is keyed by its `--tag` -- results, lane
images, and the serial-log label xtask writes into the shared target/test-logs. The per-profile
master snapshots are the one thing sweeps deliberately share, guarded by a lock that is exclusive
while a build replaces them and shared while lanes copy them.

The build phase is the exception, and unavoidably so: it writes the one build tree and the one dev
disk. Sweeps serialize against each other there, but nothing stops a bare `cargo xtask test` from
building underneath them, so prefer --reuse-images when something else is using the tree.

Serial transcripts land in target/results/many-<tag>/, named `round<N>-<config>.log`, with
`-FAILED` appended before the extension for runs that did not pass; `<same>.out` holds that run's
xtask output, since concurrent runs cannot all stream to the console.

By default a sweep runs the whole matrix. `--config` narrows it to named configurations instead,
which is what a reproducer wants -- a known-failing config hammered N times rather than a matrix
swept once. See `parse_config_spec` for the accepted spelling.
"""

import argparse
import contextlib
import fcntl
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Iterator, List, Optional, Tuple

REPO_ROOT = Path(__file__).resolve().parent

# What the default matrix sweeps. `full-debug` is deliberately not here: it is a real xtask profile
# and selectable by name, but it is slower than debug and adds a third of the matrix for no
# stability coverage the other two do not already give.
PROFILES = ("release", "debug")
ALL_PROFILES = ("release", "debug", "full-debug")
SMP_COUNTS = (1, 2, 4)

# many.py never passes --arch, so x86_64 is the only target in play.
TRIPLE = "x86_64-unknown-twizzler"
BOOT_IMAGE_DIR = REPO_ROOT / "target" / "kernel" / "x86_64-unknown-none"
DEV_DATA_IMAGE = REPO_ROOT / "target" / "disk-{}.img".format(TRIPLE)

# xtask's heartbeat cap is wall-clock, not progress-based, so emulated runs need a bigger budget
# than the 20 tries (15s each) it defaults to.
DEFAULT_SLOW_HEARTBEAT_TRIES = 160

# Seconds of totally silent serial console before a run is called dead (see TWZ_SILENCE_TIMEOUT).
KVM_SILENCE_TIMEOUT = 180
SLOW_SILENCE_TIMEOUT = 300

WORK_ROOT = REPO_ROOT / "target" / "many-work"

# A sweep that runs the disk out mid-flight fails every remaining run for a reason that looks
# nothing like the cause, so stop while there is still room to notice.
DEFAULT_MIN_FREE_GB = 15.0

RESULT_RE = re.compile(
    r"TEST RESULTS: (\d+) passed, (\d+) failed, (\d+) total -- time: ([\d.]+) seconds"
)

# Every lane thread writes status to one stdout, and `print()` emits the text and the newline as
# two separate writes -- so another lane can land between them and produce a torn line like
# `[lane 1] start  foo[lane 2] start  bar`. The run itself is fine, but anything parsing this log
# (check-many.py, a monitor grepping for FAIL) then mis-reads it. One write of one
# already-terminated string is atomic on a Python stream; the lock also keeps ordering sane.
_emit_lock = threading.Lock()


def emit(message: str) -> None:
    """Write one whole line to stdout atomically. Use this from anything a lane thread runs."""
    with _emit_lock:
        sys.stdout.write(message + "\n")
        sys.stdout.flush()


@dataclass(frozen=True)
class Config:
    profile: str
    kvm: bool
    smp: int

    @property
    def name(self) -> str:
        return f"{self.profile}-{'kvm' if self.kvm else 'nokvm'}-smp{self.smp}"


@dataclass
class Result:
    round_no: int
    config: Config
    passed: bool
    exit_code: int
    duration: float
    summary: str
    log: Optional[Path]
    lane: int = -1


@dataclass
class BuildPhase:
    profile: str
    ok: bool
    duration: float


@dataclass
class Lane:
    """One concurrent slot: a private pair of images, a port, and whatever profile it holds now."""

    index: int
    directory: Path
    # 0 asks xtask to allocate dynamically, which is what keeps concurrent sweeps from having to
    # agree on a port range.
    port: int
    profile: Optional[str] = None

    @property
    def boot_image(self) -> Path:
        return self.directory / "boot.img"

    @property
    def data_image(self) -> Path:
        return self.directory / "data.img"


def is_slow_config(config: Config) -> bool:
    """A debug build under emulation: the one quadrant an order of magnitude slower than the rest."""
    return config.profile in ("debug", "full-debug") and not config.kvm


def configurations() -> List[Config]:
    return [
        Config(profile, kvm, smp)
        for profile in PROFILES
        for kvm in (True, False)
        for smp in SMP_COUNTS
    ]


# --- configuration selection --------------------------------------------------------------------

# Aliases, not renames. `Config.name` stays `<profile>-{kvm,nokvm}-smp<N>`, because it names result
# files, the serial-log label, and everything check-many.py parses -- so the selector is where
# alternate spellings live and the stored form never moves.
_PROFILE_TOKENS = {p: p for p in ALL_PROFILES}
_ACCEL_TOKENS = {
    "kvm": True,
    "qemu-kvm": True,
    "nokvm": False,
    "tcg": False,
    "qemu-tcg": False,
}
# Both spellings: `smp4` matches how a config prints, a bare `4` is what you type.
_SMP_TOKENS = {**{f"smp{n}": n for n in SMP_COUNTS}, **{str(n): n for n in SMP_COUNTS}}


def _take(tokens: List[str], vocab: Dict[str, object]) -> Tuple[Optional[object], List[str]]:
    """Consume the longest known token from the front of `tokens`, or nothing.

    Longest-first because both `full-debug` and `qemu-kvm` are two `-`-separated words, so a plain
    per-word split cannot tell `full-debug-kvm` from a profile called `full`. No accel or smp token
    is also a profile token, so greedy is unambiguous here.
    """
    if tokens and tokens[0] == "*":
        return None, tokens[1:]
    for width in (2, 1):
        if len(tokens) >= width:
            key = "-".join(tokens[:width])
            if key in vocab:
                return vocab[key], tokens[width:]
    return None, tokens


def parse_config_spec(spec: str) -> List[Config]:
    """Expand one `--config` value into the configurations it names.

    The shape is `<profile>-<accel>-<smp>`, any field either omitted or `*` to mean "all":

        release-kvm-smp4     one configuration
        release-qemu-kvm-4   the same one; `qemu-kvm`/`qemu-tcg` and a bare `4` are accepted
        '*-kvm-smp4'         every profile, KVM, 4 cpus
        debug                every debug configuration
        smp1                 every configuration with one cpu

    Leading fields may be dropped as well as trailing ones, so `kvm-smp4` and `smp4` both work.
    Raises ValueError on anything it cannot account for -- a typo that silently selected nothing
    would look exactly like a sweep that found no failures.
    """
    tokens = [t for t in spec.strip().lower().split("-") if t]
    if not tokens:
        raise ValueError("empty configuration")
    rest = tokens
    profile, rest = _take(rest, _PROFILE_TOKENS)
    kvm, rest = _take(rest, _ACCEL_TOKENS)
    smp, rest = _take(rest, _SMP_TOKENS)
    if rest:
        raise ValueError(f"unrecognized in {spec!r}: {'-'.join(rest)!r}")

    profiles = [profile] if profile is not None else list(ALL_PROFILES)
    accels = [kvm] if kvm is not None else [True, False]
    smps = [smp] if smp is not None else list(SMP_COUNTS)
    return [Config(p, a, s) for p in profiles for a in accels for s in smps]


def select_configurations(specs: List[str]) -> List[Config]:
    """Union of every `--config`, deduplicated, in a stable order."""
    seen = {}
    for spec in specs:
        for config in parse_config_spec(spec):
            seen[config] = None
    order = {c: i for i, c in enumerate(
        Config(p, a, s) for p in ALL_PROFILES for a in (True, False) for s in SMP_COUNTS
    )}
    return sorted(seen, key=lambda c: order[c])


def config_vocabulary() -> str:
    return (
        f"profile: {', '.join(ALL_PROFILES)}; "
        f"accel: {', '.join(sorted(_ACCEL_TOKENS))}; "
        f"smp: {', '.join(f'smp{n}' for n in SMP_COUNTS)}; '*' for any"
    )


def build_jobs(args: argparse.Namespace) -> List[Tuple[int, Config]]:
    """Round-major order, so a full sweep of every configuration completes before round 2 starts.

    Takes the whole namespace rather than the three fields it needs, because check-many.py rebuilds
    a running sweep's schedule by calling this with a re-parsed argv -- and a signature that grows a
    field silently breaks that instead of failing loudly.
    """
    if args.config:
        # Naming a configuration is the opt-in, so the matrix's own gates -- --enable-slow-debug and
        # the separate --slow-rounds budget -- do not apply. Asking for N runs of one config and
        # getting one because it happened to be a TCG debug build is not a useful default.
        selected = select_configurations(args.config)
        return [(n, c) for n in range(1, args.rounds + 1) for c in selected]

    jobs = []
    for round_no in range(1, max(args.rounds, args.slow_rounds) + 1):
        for config in configurations():
            if config.kvm:
                limit = args.rounds
            elif config.profile == "debug" and not args.enable_slow_debug:
                limit = 0
            else:
                limit = args.slow_rounds
            if round_no <= limit:
                jobs.append((round_no, config))
    return jobs


# --- images -------------------------------------------------------------------------------------


def default_tag() -> str:
    """Names one invocation. Concurrent sweeps must not share results, lanes, or log labels."""
    return f"{time.strftime('%m%d-%H%M%S')}-{os.getpid()}"


def dev_boot_image(profile: str) -> Path:
    """Where `xtask make-image` writes, and what we snapshot out of the build tree."""
    return BOOT_IMAGE_DIR / profile / "disk.img"


def master_boot_image(work: Path, profile: str) -> Path:
    return work / "images" / f"{profile}-boot.img"


def master_data_image(work: Path, profile: str) -> Path:
    return work / "images" / f"{profile}-data.img"


@contextlib.contextmanager
def master_lock(work: Path, exclusive: bool) -> Iterator[None]:
    """Guard the masters, which concurrent sweeps deliberately share rather than duplicate.

    Snapshotting takes it exclusively; lane copies take it shared, so sweeps only serialize against
    a build in progress and not against each other. `copy_image` renames into place, so a reader
    never sees a half-written file -- the lock is what stops it pairing a new boot image with an
    old data image mid-snapshot.
    """
    work.mkdir(parents=True, exist_ok=True)
    with (work / ".images.lock").open("w") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def copy_image(src: Path, dst: Path) -> None:
    """Copy preserving holes -- these images are 100GB apparent over ~3GB of real data."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    tmp = dst.with_suffix(dst.suffix + ".partial")
    tmp.unlink(missing_ok=True)
    subprocess.run(
        ["cp", "--reflink=auto", "--sparse=always", str(src), str(tmp)],
        check=True,
    )
    tmp.replace(dst)


def free_gb(path: Path) -> float:
    probe = path if path.exists() else REPO_ROOT
    return shutil.disk_usage(probe).free / 2**30


class OutOfSpace(Exception):
    """Raised instead of letting a copy fail partway and look like an unrelated run failure."""


def prune_dead_lanes(work: Path, keep_tag: str) -> int:
    """Reclaim lane images left behind by sweeps that were killed rather than interrupted.

    Ownership is a held flock, not a heuristic: a live sweep keeps `.owner` locked for its whole
    run, so anything we can lock belongs to a process that is gone. Age or mtime would race with a
    sweep that is between runs and holding no qemu open.
    """
    lanes = work / "lanes"
    if not lanes.is_dir():
        return 0
    reclaimed = 0
    for tag_dir in lanes.iterdir():
        if not tag_dir.is_dir() or tag_dir.name == keep_tag:
            continue
        owner = tag_dir / ".owner"
        if not owner.exists():
            continue
        try:
            with owner.open("r+") as handle:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except (OSError, BlockingIOError):
            continue  # still running
        shutil.rmtree(tag_dir, ignore_errors=True)
        reclaimed += 1
    return reclaimed


def kill_stray_qemu(lane_root: Path) -> int:
    """Kill any qemu still holding this sweep's lane images, and report how many there were.

    xtask spawns qemu, so qemu is this driver's *grandchild*: terminating the lanes' xtask processes
    on interrupt leaves it running, reparented to init. That is not merely untidy. A stray qemu runs
    a full vcpu set flat out and holds a write lock on its lane's disk, so the next sweep starts on a
    machine that is quietly oversubscribed -- and since these sweeps exist to measure failure
    *rates*, the resulting timeouts on slow tests are indistinguishable from product bugs. One
    session lost two sweeps to twelve strays burning eleven cores between them.

    Matched on the lane root in the qemu command line, which contains this sweep's tag, so a
    concurrent sweep's processes are never touched.
    """
    needle = str(lane_root).encode()
    killed = 0
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            cmdline = (entry / "cmdline").read_bytes()
        except OSError:
            continue  # exited between listing and reading
        if b"qemu-system" not in cmdline or needle not in cmdline:
            continue
        try:
            os.kill(int(entry.name), signal.SIGKILL)
            killed += 1
        except OSError:
            pass
    return killed


def disk_usage_note(work: Path, lanes: int, profiles: Tuple[str, ...]) -> str:
    """Rough space needed: two masters per profile in play plus a private pair per lane."""
    per_pair = 0
    for profile in profiles:
        src = dev_boot_image(profile)
        if src.exists():
            per_pair = max(per_pair, src.stat().st_blocks * 512)
    data = DEV_DATA_IMAGE.stat().st_blocks * 512 if DEV_DATA_IMAGE.exists() else 0
    need = (per_pair + data) * (len(profiles) + lanes)
    free = shutil.disk_usage(work.parent if work.parent.exists() else REPO_ROOT).free
    return f"~{need / 2**30:.0f}GB of copies against {free / 2**30:.0f}GB free"


# --- building -----------------------------------------------------------------------------------


def build_command_for(profile: str) -> List[str]:
    return ["cargo", "xtask", "make-image", "--tests", "--profile", profile]


def xtask_binary() -> List[str]:
    """Prefer the built binary over `cargo xtask` so parallel runs don't queue on cargo's lock."""
    built = REPO_ROOT / ".target-xtask" / "release" / "xtask"
    return [str(built)] if built.is_file() else ["cargo", "xtask"]


def build_and_snapshot(profile: str, work: Path, args: argparse.Namespace) -> BuildPhase:
    """Build a profile, then copy its boot image and the ext4 disk out of the build tree.

    The data snapshot has to happen here, right after this profile's build: `copy_twizzler_build`
    writes the built binaries into /sysroot/pkg of the one shared disk, so the next profile's build
    would overwrite them.
    """
    start = time.monotonic()
    # Exclusive for the whole phase: the build writes the one shared build tree and the one shared
    # dev disk, so two sweeps building at once would corrupt each other's /sysroot/pkg.
    with master_lock(work, exclusive=True):
        cmd = build_command_for(profile)
        args.results_dir.mkdir(parents=True, exist_ok=True)
        out_path = args.results_dir / f"build-{profile}.out"
        print(f"=== building {profile} (log: {rel(out_path)})", flush=True)
        print(f"    $ {' '.join(cmd)}", flush=True)

        # A build is thousands of lines and says nothing a status line doesn't, so it goes to a file
        # unless asked for. Streaming it would also interleave with the lanes' status output.
        proc = subprocess.run(
            cmd, cwd=REPO_ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, errors="replace",
        )
        out_path.write_text(proc.stdout or "")
        if args.verbose:
            sys.stdout.write(proc.stdout or "")
        if proc.returncode != 0:
            print(f"BUILD FAILED ({profile}): exit {proc.returncode} -- see {rel(out_path)}",
                  file=sys.stderr, flush=True)
            # The reason is in the file, but a build error the user cannot see is a bad default.
            for line in (proc.stdout or "").splitlines():
                if line.startswith("error"):
                    print(f"    {line}", file=sys.stderr, flush=True)
            return BuildPhase(profile, False, time.monotonic() - start)

        print(f"=== snapshotting {profile} images", flush=True)
        try:
            copy_image(dev_boot_image(profile), master_boot_image(work, profile))
            copy_image(DEV_DATA_IMAGE, master_data_image(work, profile))
        except (OSError, subprocess.CalledProcessError) as e:
            print(f"SNAPSHOT FAILED ({profile}): {e}", file=sys.stderr, flush=True)
            return BuildPhase(profile, False, time.monotonic() - start)

    return BuildPhase(profile, True, time.monotonic() - start)


def masters_present(work: Path, profile: str) -> bool:
    return master_boot_image(work, profile).is_file() and master_data_image(work, profile).is_file()


# --- running ------------------------------------------------------------------------------------


def command_for(config: Config, lane: Lane, label: str, serial_log: Path) -> List[str]:
    return xtask_binary() + [
        "test",
        "--scenario",
        "default",
        "--profile",
        config.profile,
        "--enable-kvm" if config.kvm else "--disable-kvm",
        # Straight into this sweep's directory, rather than transiting the shared target/test-logs
        # where a sweep that dies mid-run would strand it.
        "--serial-log",
        str(serial_log),
        # Private copies: nothing here refers to the build tree, so a sweep and interactive
        # development can run at the same time.
        "--boot-image",
        str(lane.boot_image),
        "--disk-image",
        str(lane.data_image),
        "--label",
        label,
        "--ssh-port",
        str(lane.port),
        # -smp is not an xtask flag, so it goes through to qemu directly. --nographic keeps the run
        # headless, matching what CI passes.
        "--qemu-options=-smp",
        f"--qemu-options={config.smp}",
        "--qemu-options=--nographic",
    ]


def env_for(config: Config, args: argparse.Namespace) -> Dict[str, str]:
    env = dict(os.environ)
    tries = args.heartbeat_tries if config.kvm else args.slow_heartbeat_tries
    if is_slow_config(config) and tries is not None:
        # A debug TCG run outlasts the ordinary budget while still making progress, which is what
        # used to make this quadrant report false failures. Being generous here is safe because the
        # heartbeat cap is not what detects death -- the silence watchdog below is, and it fires on
        # a guest that has stopped producing output regardless of how long the run is allowed.
        tries *= args.slow_debug_factor
    if tries is not None:
        env["TWZ_HEARTBEAT_TRIES"] = str(tries)
    # A dead guest stops printing, which is the only death signal that survives concurrent cpus
    # garbling the console. Emulated runs are slow enough that a legitimate quiet stretch is longer,
    # so give them more slack than the xtask default -- and more again when lanes are competing for
    # cpu, since these budgets are wall-clock and a contended run genuinely does pause longer.
    base = SLOW_SILENCE_TIMEOUT if not config.kvm else KVM_SILENCE_TIMEOUT
    env["TWZ_SILENCE_TIMEOUT"] = str(int(base * args.timeout_scale))
    return env


class Tee:
    """Mirror the driver's status lines into the sweep's own directory as well as the console.

    Lanes print from their own threads, so the lock is what keeps two status lines from interleaving
    mid-write.
    """

    def __init__(self, console, handle):
        self.console = console
        self.handle = handle
        self.lock = threading.Lock()

    def write(self, text: str) -> int:
        with self.lock:
            self.handle.write(text)
            self.handle.flush()
            return self.console.write(text)

    def flush(self) -> None:
        with self.lock:
            self.handle.flush()
            self.console.flush()


def summarize(output: List[str], exit_code: int) -> str:
    for line in reversed(output):
        match = RESULT_RE.search(line)
        if match:
            npass, nfail, total, _ = match.groups()
            return f"{npass}/{total} tests passed, {nfail} failed"
    if exit_code == 35:
        return "guest kernel panicked"
    if exit_code == 36:
        return "guest went silent (hung or garbled panic)"
    if exit_code == 37:
        return "guest livelocked (printing, but nothing new)"
    if exit_code == 34:
        return "no test report (timeout or early exit)"
    return f"no test report (exit {exit_code})"


def store_log(
    name: str, serial: Path, passed: bool, output: List[str], results_dir: Path
) -> Optional[Path]:
    results_dir.mkdir(parents=True, exist_ok=True)
    suffix = "" if passed else "-FAILED"
    dest = results_dir / f"{name}{suffix}.log"

    # Concurrent runs can't share the console, so every run's xtask output goes to a file next to
    # its transcript rather than being dropped.
    (results_dir / f"{name}{suffix}.out").write_text("".join(output))

    if serial.exists():
        if serial != dest:
            serial.replace(dest)
        return dest

    # No transcript means the run never got as far as a monitored boot (qemu refused to start, ...).
    with dest.open("w") as f:
        f.write("(no serial transcript produced; xtask output follows)\n\n")
        f.writelines(output)
    return dest


def sync_lane(lane: Lane, work: Path, profile: str, min_free_gb: float) -> None:
    """Give a lane private images for `profile`, reusing what it already has when it can.

    Reuse matters for more than speed: a lane that keeps its data image across runs reproduces the
    existing behaviour where guest-written disk state carries from one run to the next.
    """
    if lane.profile == profile:
        return
    if free_gb(lane.directory) < min_free_gb:
        raise OutOfSpace(
            f"only {free_gb(lane.directory):.0f}GB free, below the {min_free_gb:.0f}GB floor"
        )
    lane.directory.mkdir(parents=True, exist_ok=True)
    # Shared: concurrent sweeps copy at the same time, but none of them reads a master that a build
    # is midway through replacing.
    with master_lock(work, exclusive=False):
        copy_image(master_boot_image(work, profile), lane.boot_image)
        copy_image(master_data_image(work, profile), lane.data_image)
    lane.profile = profile


def run_once(
    round_no: int,
    config: Config,
    lane: Lane,
    work: Path,
    args: argparse.Namespace,
    live: Dict[int, subprocess.Popen],
    live_lock: threading.Lock,
) -> Result:
    # Two names: `name` files the results inside this sweep's own directory, while `label` names the
    # serial log xtask writes into the shared target/test-logs, where concurrent sweeps would
    # otherwise overwrite each other.
    name = f"round{round_no}-{config.name}"
    label = f"{args.tag}-{name}"
    start = time.monotonic()

    try:
        sync_lane(lane, work, config.profile, args.min_free_gb)
    except OutOfSpace as e:
        return Result(round_no, config, False, -1, time.monotonic() - start,
                      f"out of disk: {e}", None, lane.index)
    except (OSError, subprocess.CalledProcessError) as e:
        return Result(round_no, config, False, -1, time.monotonic() - start,
                      f"image copy failed: {e}", None, lane.index)

    args.results_dir.mkdir(parents=True, exist_ok=True)
    serial = args.results_dir / f"{name}.log"
    serial.unlink(missing_ok=True)
    cmd = command_for(config, lane, label, serial)
    emit(f"[lane {lane.index}] start  {name}")

    output: List[str] = []
    proc = subprocess.Popen(
        cmd,
        cwd=REPO_ROOT,
        env=env_for(config, args),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
    )
    with live_lock:
        live[lane.index] = proc
    try:
        assert proc.stdout is not None
        for line in proc.stdout:
            output.append(line)
            if args.verbose:
                emit(f"[lane {lane.index}] {line.rstrip(chr(10))}")
        exit_code = proc.wait()
    finally:
        with live_lock:
            live.pop(lane.index, None)

    duration = time.monotonic() - start
    passed = exit_code == 0
    log = store_log(name, serial, passed, output, args.results_dir)
    summary = summarize(output, exit_code)
    emit(
        f"[lane {lane.index}] {'PASS' if passed else 'FAIL'}   {name}  "
        f"{duration / 60:.1f}m  {summary}"
    )
    return Result(round_no, config, passed, exit_code, duration, summary, log, lane.index)


# --- reporting ----------------------------------------------------------------------------------


def rel(path: Path) -> str:
    """Repo-relative if it can be, absolute otherwise — a results dir need not live under the repo."""
    try:
        return str(path.resolve().relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def report(
    results: List[Result], jobs_total: int, builds: List[BuildPhase], wall: float
) -> None:
    print("\n" + "=" * 78)
    print(f"RESULTS ({len(results)} of {jobs_total} runs completed)")
    print("=" * 78)

    ordered = sorted(results, key=lambda r: (r.round_no, r.config.name))
    width = max((len(r.config.name) for r in ordered), default=10)
    lines = []
    for r in ordered:
        lines.append(
            f"round {r.round_no}  {r.config.name:<{width}}  "
            f"{'PASS' if r.passed else 'FAIL'}  {r.duration / 60:6.1f}m  "
            f"exit {r.exit_code:<3}  {r.summary}"
            + (f"  [{rel(r.log)}]" if r.log else "")
        )
    for line in lines:
        print(line)

    build_time = sum(b.duration for b in builds)
    run_time = sum(r.duration for r in results)
    lines.append("")
    lines.append(
        f"{len(builds)} builds ({build_time / 60:.1f}m) + {len(results)} runs "
        f"({run_time / 60:.1f}m of run time in {wall / 60:.1f}m wall)"
    )
    for b in builds:
        lines.append(
            f"    build {b.profile:<8} {'ok' if b.ok else 'FAILED'}  {b.duration / 60:6.1f}m"
        )
    for line in lines[-(len(builds) + 2):]:
        print(line)

    failures = [r for r in ordered if not r.passed]
    passes = len(ordered) - len(failures)

    # Across several rounds the per-run list is too long to read, and the question a stability
    # sweep is asking is which configuration fails and how often -- so total it up per config.
    by_config: Dict[str, List[Result]] = {}
    for r in ordered:
        by_config.setdefault(r.config.name, []).append(r)
    if any(len(v) > 1 for v in by_config.values()):
        table_start = len(lines)
        lines.append("")
        lines.append("per-configuration outcomes:")
        for name in sorted(by_config, key=lambda k: sum(not r.passed for r in by_config[k]),
                           reverse=True):
            runs = by_config[name]
            bad = [r for r in runs if not r.passed]
            reasons: Dict[str, int] = {}
            for r in bad:
                reasons[r.summary] = reasons.get(r.summary, 0) + 1
            detail = ("  " + ", ".join(f"{s} (x{n})" if n > 1 else s
                                       for s, n in sorted(reasons.items(), key=lambda kv: -kv[1])))
            lines.append(
                f"    {name:<{width}}  {len(runs) - len(bad)}/{len(runs)} passed"
                + (detail if bad else "")
            )
        for line in lines[table_start:]:
            print(line)

    print()
    print(f"{passes} passed, {len(failures)} failed")
    if failures:
        print("failing runs:")
        for r in failures:
            print(f"    round {r.round_no}  {r.config.name}: {r.summary}")

    # Runs that never produced a log (a failed image copy, say) leave `log` unset, so find any run
    # that did rather than assuming the first one has one.
    logged = next((r.log for r in ordered if r.log), None)
    if logged:
        summary_path = logged.parent / "summary.txt"
        summary_path.write_text("\n".join(lines) + "\n")
        print(f"\nsummary written to {rel(summary_path)}")


# --- main ---------------------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    """The sweep's command line.

    A function rather than inline in `main` so check-many.py can rebuild a running sweep's schedule
    from its argv: an ETA needs to know what is still pending, and a second copy of these defaults
    would be wrong the moment one of them changed here.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("-r", "--rounds", type=int, default=1,
                        help="Times to run each KVM-accelerated configuration (default: 1).")
    parser.add_argument("--config", action="append", default=[], metavar="SPEC",
                        help="Run only the named configuration(s) instead of the whole matrix; "
                             "repeatable. SPEC is <profile>-<accel>-<smp>, any field omitted or "
                             "'*' meaning all -- e.g. release-kvm-smp4, '*-kvm-smp4', debug, "
                             "smp1. Naming a configuration is the opt-in, so --slow-rounds and "
                             "--enable-slow-debug do not apply: every selected configuration runs "
                             "--rounds times. Vocabulary -- " + config_vocabulary() + ".")
    parser.add_argument("--slow-rounds", type=int, default=1,
                        help="Times to run each non-KVM configuration, which is far slower "
                             "(default: 1). 0 skips them entirely. Ignored with --config.")
    parser.add_argument("--enable-slow-debug", action="store_true",
                        help="Include debug+non-KVM configurations, the slowest quadrant. Their "
                             "heartbeat budget is scaled automatically (--slow-debug-factor) and "
                             "at most --max-slow of them run at once.")
    parser.add_argument("--max-slow", type=int, default=None,
                        help="Cap how many debug+TCG runs execute at once, so they cannot occupy "
                             "every lane (default: half the lanes, at least one). 0 removes the "
                             "cap.")
    parser.add_argument("--slow-debug-factor", type=int, default=3,
                        help="Multiply the non-KVM heartbeat budget by this for debug+TCG runs "
                             "(default: 3). The silence watchdog, not this cap, is what detects a "
                             "dead guest.")
    parser.add_argument("-j", "--jobs", type=int, default=4,
                        help="Concurrent lanes (default: 4). Each holds a private image pair and "
                             "one qemu; smp4 lanes take 4 vcpus apiece.")
    parser.add_argument("--tag", default=None,
                        help="Names this sweep, keeping its results, lanes and serial-log labels "
                             "clear of any other sweep running at the same time (default: "
                             "timestamp-pid).")
    parser.add_argument("--results-dir", type=Path, default=None,
                        help="Where to write serial logs (default: target/results/many-<tag>).")
    parser.add_argument("--work-dir", type=Path, default=WORK_ROOT,
                        help=f"Where image copies live (default: {rel(WORK_ROOT)}). The per-profile "
                             "masters here are shared between sweeps under a lock; lanes are "
                             "per-sweep.")
    parser.add_argument("--reuse-images", action="store_true",
                        help="Skip the build and reuse the snapshots already in --work-dir. Fast "
                             "path for re-running a sweep against an unchanged tree.")
    parser.add_argument("--keep-lanes", action="store_true",
                        help="Leave lane image copies behind for inspection instead of deleting "
                             "them when the sweep ends.")
    parser.add_argument("--min-free-gb", type=float, default=DEFAULT_MIN_FREE_GB,
                        help=f"Refuse to copy a lane's images below this much free space (default: "
                             f"{DEFAULT_MIN_FREE_GB:g}GB). Lane data images grow as guests write, "
                             "so a long sweep can run the disk down.")
    parser.add_argument("--port-base", type=int, default=0,
                        help="First host ssh-forward port; lane N uses base+N. Default 0 lets qemu "
                             "pick, so concurrent sweeps need no agreed port range -- set this only "
                             "when you want to know a guest's port in advance.")
    parser.add_argument("--heartbeat-tries", type=int, default=None,
                        help="Override TWZ_HEARTBEAT_TRIES (15s each) for KVM runs; default is "
                             "xtask's own.")
    parser.add_argument("--slow-heartbeat-tries", type=int, default=DEFAULT_SLOW_HEARTBEAT_TRIES,
                        help=f"TWZ_HEARTBEAT_TRIES for non-KVM runs (default: "
                             f"{DEFAULT_SLOW_HEARTBEAT_TRIES}).")
    parser.add_argument("--timeout-scale", type=float, default=None,
                        help="Multiply the silence timeouts by this. Defaults to scaling with "
                             "--jobs, since those budgets are wall-clock and contended runs "
                             "legitimately pause longer.")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="Also stream build and run output to the console. Off by default: it "
                             "is thousands of lines, it interleaves badly across lanes, and it is "
                             "written to the sweep's directory either way.")
    parser.add_argument("--dry-run", action="store_true",
                        help="List the runs that would happen, then exit.")
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    if args.jobs < 1:
        parser.error("--jobs must be at least 1")
    if args.timeout_scale is None:
        # Enough slack for lanes competing over cpu without blunting the watchdog outright.
        args.timeout_scale = max(1.0, 1.0 + (args.jobs - 1) * 0.5)
    if args.max_slow is None:
        args.max_slow = max(1, args.jobs // 2)

    try:
        jobs = build_jobs(args)
    except ValueError as e:
        parser.error(f"--config: {e}. Vocabulary -- {config_vocabulary()}.")
    if not jobs:
        print("nothing to run")
        return 0

    if args.tag is None:
        args.tag = default_tag()
    if args.results_dir is None:
        args.results_dir = REPO_ROOT / "target" / "results" / f"many-{args.tag}"

    work: Path = args.work_dir
    # Over ALL_PROFILES, not PROFILES: --config can name a profile the default matrix never sweeps,
    # and a profile that is scheduled but never built fails every run of it at image-copy time.
    needed = [p for p in ALL_PROFILES if any(c.profile == p for _, c in jobs)]
    lanes = [
        Lane(
            index=i,
            directory=work / "lanes" / args.tag / f"lane{i}",
            port=(args.port_base + i) if args.port_base else 0,
        )
        for i in range(min(args.jobs, len(jobs)))
    ]

    if args.dry_run:
        for profile in needed:
            action = "reuse snapshot" if args.reuse_images else " ".join(build_command_for(profile))
            print(f"                 {profile}: {action}")
        for round_no, config in jobs:
            print(f"round {round_no}  {config.name}")
        ports = (f"ports {args.port_base}..{args.port_base + len(lanes) - 1}"
                 if args.port_base else "ports assigned dynamically")
        n_slow = sum(1 for _, c in jobs if is_slow_config(c))
        print(f"\ntag {args.tag}: {len(lanes)} lanes, {ports}, "
              f"silence timeouts x{args.timeout_scale:g}")
        if n_slow:
            cap = max(1, min(args.max_slow, len(lanes))) if args.max_slow else len(lanes)
            print(f"{n_slow} debug+TCG runs, at most {cap} at once, "
                  f"heartbeat x{args.slow_debug_factor}")
        print(f"results  {rel(args.results_dir)}")
        print(f"lanes    {rel(work / 'lanes' / args.tag)}")
        print(disk_usage_note(work, len(lanes), tuple(needed)))
        return 0

    work.mkdir(parents=True, exist_ok=True)
    args.results_dir.mkdir(parents=True, exist_ok=True)
    driver_log = (args.results_dir / "driver.log").open("w")
    sys.stdout = Tee(sys.stdout, driver_log)

    # Held for the whole sweep so a later run can tell our lanes from a dead sweep's leftovers.
    lane_root = work / "lanes" / args.tag
    lane_root.mkdir(parents=True, exist_ok=True)
    owner = (lane_root / ".owner").open("w")
    fcntl.flock(owner, fcntl.LOCK_EX)

    reclaimed = prune_dead_lanes(work, args.tag)
    if reclaimed:
        print(f"reclaimed lane images from {reclaimed} dead sweep(s)", flush=True)

    if free_gb(work) < args.min_free_gb:
        print(f"only {free_gb(work):.0f}GB free, below the {args.min_free_gb:.0f}GB floor; "
              "free space or lower --min-free-gb", file=sys.stderr, flush=True)
        return 1

    started = time.monotonic()
    builds: List[BuildPhase] = []
    broken = set()

    # Build phase: strictly sequential, since every profile builds through the same target dir and
    # the same shared ext4 disk. Everything after this point runs off private copies.
    for profile in needed:
        if args.reuse_images:
            if masters_present(work, profile):
                print(f"=== reusing {profile} snapshot", flush=True)
                continue
            print(f"no {profile} snapshot in {rel(work)}; building it", flush=True)
        phase = build_and_snapshot(profile, work, args)
        builds.append(phase)
        if not phase.ok:
            broken.add(profile)

    runnable = [(n, c) for n, c in jobs if c.profile not in broken]
    results: List[Result] = [
        Result(n, c, False, -1, 0.0, "skipped: build failed", None)
        for n, c in jobs
        if c.profile in broken
    ]
    if not runnable:
        report(results, len(jobs), builds, time.monotonic() - started)
        return 1

    n_slow = sum(1 for _, c in runnable if is_slow_config(c))
    slow_note = (f", {n_slow} debug+TCG capped at {max(1, min(args.max_slow, len(lanes)))} at once"
                 if n_slow and args.max_slow else "")
    print(f"\ntag {args.tag}: running {len(runnable)} configurations across {len(lanes)} lanes"
          f"{slow_note} ({disk_usage_note(work, len(lanes), tuple(needed))})", flush=True)
    print(f"results -> {rel(args.results_dir)}", flush=True)

    pending: List[Tuple[int, Config]] = list(runnable)
    slow_cap = max(1, min(args.max_slow, len(lanes))) if args.max_slow else len(lanes)
    slow_running = 0

    results_lock = threading.Lock()
    live: Dict[int, subprocess.Popen] = {}
    live_lock = threading.Lock()
    stop = threading.Event()
    sched = threading.Condition()

    def take(prefer_slow: bool) -> Optional[Tuple[int, Config]]:
        """Hand out the next job, biased by the calling lane's preference.

        The slow configurations sort last, so a plain queue would leave them all to the end and
        finish with most lanes idle. Preferring them everywhere fixes that but overcorrects: every
        lane grabs a slow job immediately and the first fast result is an hour away. So lanes split
        -- some prefer slow and fall back to fast, the rest the other way around -- which keeps the
        slow tail busy from the start while still getting KVM results back in the first minutes.

        Either preference takes the other kind rather than idling, so no lane ever waits while
        there is takeable work.
        """
        nonlocal slow_running
        with sched:
            while not stop.is_set():
                if not pending:
                    return None
                slow_at = next((i for i, (_, c) in enumerate(pending) if is_slow_config(c)), None)
                # `is_slow_config` is binary, but there are three cost tiers, and release-nokvm
                # lands on the fast side despite being emulated. Picking the first "fast" job in
                # round-major order therefore takes release-nokvm ahead of debug-kvm and leaves
                # every lane on TCG with the accelerator idle. Rank by cost instead.
                kvm_at = next((i for i, (_, c) in enumerate(pending) if c.kvm), None)
                fast_at = next((i for i, (_, c) in enumerate(pending)
                                if not is_slow_config(c)), None)
                slow_ok = slow_at if (slow_at is not None and slow_running < slow_cap) else None
                order = (slow_ok, kvm_at, fast_at) if prefer_slow else (kvm_at, fast_at, slow_ok)
                pick = next((i for i in order if i is not None), None)
                if pick is not None:
                    pass
                elif slow_at is not None:
                    # Only slow jobs left and the cap is full. The cap exists to keep them from
                    # starving fast runs, and there are none left to starve, so holding lanes idle
                    # here buys nothing. Use --jobs to bound load.
                    pick = slow_at
                else:
                    sched.wait(timeout=1.0)
                    continue
                job = pending.pop(pick)
                if is_slow_config(job[1]):
                    slow_running += 1
                return job
            return None

    def release(config: Config) -> None:
        nonlocal slow_running
        with sched:
            if is_slow_config(config):
                slow_running -= 1
            sched.notify_all()

    # Lower-numbered lanes chase the slow tail; the rest keep fast results flowing. Both take
    # either kind once their preferred sort is exhausted.
    slow_lanes = max(1, len(lanes) // 2)

    def worker(lane: Lane) -> None:
        while not stop.is_set():
            job = take(prefer_slow=lane.index < slow_lanes)
            if job is None:
                return
            round_no, config = job
            try:
                result = run_once(round_no, config, lane, work, args, live, live_lock)
            except Exception as e:  # a lane dying must not silently drop its job
                result = Result(round_no, config, False, -1, 0.0, f"lane error: {e}", None,
                                lane.index)
            release(config)
            with results_lock:
                results.append(result)

    threads = [threading.Thread(target=worker, args=(lane,), daemon=True) for lane in lanes]
    for t in threads:
        t.start()
    try:
        while any(t.is_alive() for t in threads):
            for t in threads:
                t.join(timeout=0.5)
    except KeyboardInterrupt:
        print("\ninterrupted; stopping lanes", file=sys.stderr, flush=True)
        stop.set()
        with live_lock:
            for proc in live.values():
                proc.terminate()
        for t in threads:
            t.join(timeout=30)
        with live_lock:
            for proc in live.values():
                proc.kill()
        report(results, len(jobs), builds, time.monotonic() - started)
        return 130
    finally:
        # Before the rmtree, and unconditionally: a stray qemu outlives --keep-lanes too, and
        # deleting images out from under a running qemu is its own kind of confusing.
        strays = kill_stray_qemu(lane_root)
        if strays:
            print(f"killed {strays} stray qemu process(es) still holding this sweep's lanes",
                  flush=True)
        if not args.keep_lanes:
            for lane in lanes:
                shutil.rmtree(lane.directory, ignore_errors=True)
            with contextlib.suppress(OSError):
                (work / "lanes" / args.tag).rmdir()

    report(results, len(jobs), builds, time.monotonic() - started)
    return 0 if results and all(r.passed for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
