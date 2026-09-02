use std::{
    collections::BTreeMap,
    io::{Write, stdout},
    time::{Duration, Instant},
};

use crossterm::{
    ExecutableCommand, QueueableCommand,
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use monitor_api::CompartmentHandle;
use twizzler::object::{Object, TypedObject};
use twizzler_abi::{
    object::ObjID,
    syscall::{
        EnumerateKind, KernelStats, MemoryStats, ThreadSchedStats, ThreadSctxIds, sys_info,
        sys_kernel_stats, sys_memory_stats, sys_object_enumerate_notes, sys_object_get_note,
        sys_thread_read_sctx_ids, sys_thread_read_stats, sys_thread_self_id, sys_thread_stats,
    },
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

const REFRESH: Duration = Duration::from_millis(1000);

/// Which thread rows to draw. Compartment lines are always drawn -- hiding threads is what
/// turns this display into a per-compartment summary, so the summary has to survive it.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum ThreadView {
    #[default]
    All,
    /// Hide threads belonging to no compartment: kernel threads, and the userspace ones with a
    /// zero home (the monitor's own, bootstrap, statically-linked programs). Defined by the
    /// repr flag and the home context rather than by a list of server names, so nothing has to
    /// be kept in sync as compartments come and go.
    HideSystem,
    HideAll,
}

impl ThreadView {
    fn next(&self) -> Self {
        match self {
            ThreadView::All => ThreadView::HideSystem,
            ThreadView::HideSystem => ThreadView::HideAll,
            ThreadView::HideAll => ThreadView::All,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            ThreadView::All => "all threads",
            ThreadView::HideSystem => "hiding system threads",
            ThreadView::HideAll => "compartments only",
        }
    }
}

/// Column the display is ordered by. Groups are ordered by the same key, using the group's
/// aggregate of it, so the ordering the user picked holds at both levels.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum SortKey {
    #[default]
    Cpu,
    User,
    Sys,
    Faults,
    Pager,
    Syscalls,
    Wakes,
    Time,
    Name,
    State,
    Id,
}

impl SortKey {
    fn next(&self) -> Self {
        match self {
            SortKey::Cpu => SortKey::User,
            SortKey::User => SortKey::Sys,
            SortKey::Sys => SortKey::Faults,
            SortKey::Faults => SortKey::Pager,
            SortKey::Pager => SortKey::Syscalls,
            SortKey::Syscalls => SortKey::Wakes,
            SortKey::Wakes => SortKey::Time,
            SortKey::Time => SortKey::Name,
            SortKey::Name => SortKey::State,
            SortKey::State => SortKey::Id,
            SortKey::Id => SortKey::Cpu,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            SortKey::Cpu => "cpu",
            SortKey::User => "user",
            SortKey::Sys => "sys",
            SortKey::Faults => "faults",
            SortKey::Pager => "pager",
            SortKey::Syscalls => "syscalls",
            SortKey::Wakes => "wakes",
            SortKey::Time => "time",
            SortKey::Name => "name",
            SortKey::State => "state",
            SortKey::Id => "id",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "cpu" => SortKey::Cpu,
            "user" => SortKey::User,
            "sys" | "system" => SortKey::Sys,
            "faults" | "flt" => SortKey::Faults,
            "pager" | "pio" => SortKey::Pager,
            "syscalls" | "sc" => SortKey::Syscalls,
            "wakes" | "wake" => SortKey::Wakes,
            "time" => SortKey::Time,
            "name" => SortKey::Name,
            "state" => SortKey::State,
            "id" => SortKey::Id,
            _ => return None,
        })
    }

    /// Whether the natural reading of this column is largest-first. Costs descend; labels
    /// ascend.
    fn descends(&self) -> bool {
        !matches!(self, SortKey::Name | SortKey::Id)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let batch = args.iter().any(|a| a == "-b" || a == "--batch");

    let mut tracker = ThreadTracker::default();
    tracker.show_ids = args.iter().any(|a| a == "--ids");
    tracker.rev = args.iter().any(|a| a == "-r" || a == "--reverse");
    for arg in &args {
        if let Some(key) = arg.strip_prefix("--sort=") {
            match SortKey::parse(key) {
                Some(k) => tracker.sort = k,
                None => {
                    eprintln!("top: unknown sort key `{}`", key);
                    std::process::exit(1);
                }
            }
        }
    }
    // Sets the starting mode; the interactive display can still cycle from here with `t`.
    if args.iter().any(|a| a == "--compartments" || a == "-c") {
        tracker.view = ThreadView::HideAll;
    } else if args.iter().any(|a| a == "--hide-system" || a == "-s") {
        tracker.view = ThreadView::HideSystem;
    }

    if batch {
        // Percentages are per-interval, so one sample has nothing to compare against.
        tracker.sample();
        std::thread::sleep(REFRESH);
        tracker.sample();
        tracker.render_plain(&mut stdout()).unwrap();
        return;
    }

    enable_raw_mode().unwrap();
    let mut out = stdout();
    out.execute(EnterAlternateScreen).unwrap();
    out.execute(Hide).unwrap();

    tracker.sample();
    tracker.render(&mut out).unwrap();

    while !wait_for_input(REFRESH, &mut tracker, &mut out) {
        tracker.sample();
        tracker.render(&mut out).unwrap();
    }

    let _ = out.execute(Show);
    let _ = out.execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Waits out the refresh interval, returning true if the user asked to quit. A mode change is
/// redrawn immediately rather than at the next sample, so the display tracks the keypress.
fn wait_for_input(timeout: Duration, tracker: &mut ThreadTracker, out: &mut impl Write) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        if event::poll(deadline - now).unwrap() {
            if let Event::Key(key) = event::read().unwrap() {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return true,
                    KeyCode::Char('t') => {
                        tracker.view = tracker.view.next();
                        tracker.render(out).unwrap();
                    }
                    KeyCode::Char('i') => {
                        tracker.show_ids = !tracker.show_ids;
                        tracker.render(out).unwrap();
                    }
                    KeyCode::Char('s') => {
                        tracker.sort = tracker.sort.next();
                        tracker.render(out).unwrap();
                    }
                    KeyCode::Char('r') => {
                        tracker.rev = !tracker.rev;
                        tracker.render(out).unwrap();
                    }
                    _ => {}
                }
            }
        }
    }
}

struct ThreadInfo {
    id: ObjID,
    name: Option<String>,
    state: Option<ExecutionState>,
    /// Mapped once and kept. Remapping every sample churns the compartment's slots and the
    /// monitor's handle cache for a value that is one atomic load away.
    repr: Option<Object<ThreadRepr>>,
    /// Cumulative statticks, as of the last sample.
    stats: ThreadSchedStats,
    /// Statticks charged during the last sample interval.
    delta: ThreadSchedStats,
    /// True until this thread has been sampled twice; its first delta would be its whole
    /// lifetime, not an interval.
    fresh: bool,
    err: Option<TwzError>,
    seen: bool,
    /// Home and active security contexts, or None if the read failed. A thread whose active
    /// context differs from its home is inside a cross-compartment call.
    sctx: Option<ThreadSctxIds>,
    /// From the repr's flags. The security context ids cannot supply this: the kernel context
    /// and the monitor instance are both id zero.
    kernel: bool,
}

impl ThreadInfo {
    fn cpu_ticks(&self) -> u64 {
        self.stats.user.saturating_add(self.stats.system)
    }

    fn delta_cpu_ticks(&self) -> u64 {
        self.delta.user.saturating_add(self.delta.system)
    }

    /// Fraction of one cpu used over the last interval. `elapsed` is the interval in
    /// statticks; a thread cannot have run for more of it than that.
    fn cpu(&self, elapsed: u64) -> f64 {
        frac(self.delta_cpu_ticks(), elapsed)
    }

    fn user(&self, elapsed: u64) -> f64 {
        frac(self.delta.user, elapsed)
    }

    fn system(&self, elapsed: u64) -> f64 {
        frac(self.delta.system, elapsed)
    }

    /// Per-second rates over the last interval. `secs` is wall time, not statticks: these count
    /// events, so nothing bounds them by a fraction of a cpu the way the time columns are.
    fn faults(&self, secs: f64) -> f64 {
        rate(self.delta.faults, secs)
    }

    /// Pages this thread asked the pager for. A low figure does not mean "light pager user":
    /// read-ahead is charged to whoever triggered it, so a thread whose pages someone else's
    /// prefetch already brought in reads as zero here.
    fn pager(&self, secs: f64) -> f64 {
        rate(self.delta.pager_pages, secs)
    }

    fn syscalls(&self, secs: f64) -> f64 {
        rate(self.delta.syscalls, secs)
    }

    /// Wakes per second. Read against CPU%: a thread waking hundreds of times a second for
    /// nearly no cpu is being woken to find nothing to do, which is what polling looks like
    /// from the outside.
    fn wakes(&self, secs: f64) -> f64 {
        rate(self.delta.wakes, secs)
    }
}

impl ThreadInfo {
    /// The compartment this thread belongs to. Zero covers everything with no compartment:
    /// kernel threads, statically-linked ones, and the monitor's own.
    fn home(&self) -> ObjID {
        self.sctx.map(|s| s.home).unwrap_or(ObjID::new(0))
    }

    fn is_cross(&self) -> bool {
        self.sctx.is_some_and(|s| s.is_cross())
    }
}

fn rate(count: u64, secs: f64) -> f64 {
    if secs <= 0.0 {
        0.0
    } else {
        count as f64 / secs
    }
}

/// Compact fixed-width rendering for a per-second rate, so a busy thread's syscall count cannot
/// widen the column and shove everything after it off the line.
fn fmt_rate(v: f64) -> String {
    if v <= 0.0 {
        // A true zero, not "unknown" -- see [ThreadTracker::rate_str], which owns the
        // distinction. An idle system legitimately faults zero times in a second, and printing
        // that as "-" makes a working counter look broken.
        "0".to_string()
    } else if v < 1000.0 {
        format!("{:.0}", v)
    } else if v < 1_000_000.0 {
        format!("{:.0}k", v / 1000.0)
    } else {
        format!("{:.1}M", v / 1_000_000.0)
    }
}

fn frac(ticks: u64, elapsed: u64) -> f64 {
    if elapsed == 0 {
        0.0
    } else {
        (ticks as f64 / elapsed as f64).min(1.0)
    }
}

fn total(stats: &ThreadSchedStats) -> u64 {
    stats
        .user
        .saturating_add(stats.system)
        .saturating_add(stats.idle)
}

/// One compartment's worth of threads, as displayed.
struct CompGroup<'a> {
    label: String,
    threads: Vec<&'a ThreadInfo>,
    /// Statticks charged to the group over the last interval, for ordering groups.
    ticks: u64,
    user_ticks: u64,
    sys_ticks: u64,
    fault_count: u64,
    pager_count: u64,
    syscall_count: u64,
    wake_count: u64,
    /// Threads currently executing in some other compartment.
    cross: usize,
    /// The group belongs to no compartment -- see [ThreadView::HideSystem].
    system: bool,
}

impl CompGroup<'_> {
    /// Fractions of one cpu the whole group used over the last interval. Summed from the same
    /// per-thread figures the rows show, so a collapsed group still reports what it cost.
    fn cpu(&self, elapsed: u64) -> f64 {
        self.threads.iter().map(|t| t.cpu(elapsed)).sum()
    }

    fn user(&self, elapsed: u64) -> f64 {
        self.threads.iter().map(|t| t.user(elapsed)).sum()
    }

    fn system(&self, elapsed: u64) -> f64 {
        self.threads.iter().map(|t| t.system(elapsed)).sum()
    }

    fn cpu_ticks(&self) -> u64 {
        self.threads
            .iter()
            .fold(0u64, |acc, t| acc.saturating_add(t.cpu_ticks()))
    }

    fn faults(&self, secs: f64) -> f64 {
        self.threads.iter().map(|t| t.faults(secs)).sum()
    }

    fn pager(&self, secs: f64) -> f64 {
        self.threads.iter().map(|t| t.pager(secs)).sum()
    }

    fn syscalls(&self, secs: f64) -> f64 {
        self.threads.iter().map(|t| t.syscalls(secs)).sum()
    }

    fn wakes(&self, secs: f64) -> f64 {
        self.threads.iter().map(|t| t.wakes(secs)).sum()
    }

    /// Threads the kernel calls Running, which covers both on-cpu and waiting on a run queue --
    /// the state alone does not say which, so this is "not blocked" rather than "on a cpu now".
    fn running(&self) -> usize {
        self.threads
            .iter()
            .filter(|t| t.state == Some(ExecutionState::Running))
            .count()
    }

    /// The `N threads, M cross` tail, which lands in the WHERE column.
    fn tail(&self) -> String {
        let mut s = format!(
            "{} thread{}",
            self.threads.len(),
            if self.threads.len() == 1 { "" } else { "s" }
        );
        if self.cross > 0 {
            s.push_str(&format!(", {} cross", self.cross));
        }
        s
    }
}

/// The system-wide counters, taken once per sample. All cumulative except the two levels noted,
/// so everything the header shows is a difference between two of these over the wall time
/// between them.
#[derive(Copy, Clone)]
struct SysSample {
    kernel: KernelStats,
    mem: MemoryStats,
    syscalls: u64,
    /// Cumulative nanoseconds the hypervisor ran something else while our cpus were runnable.
    /// Zero on bare metal; nonzero and growing means these numbers were taken on a busy host.
    steal_ns: u64,
    at: Instant,
}

impl SysSample {
    /// Two syscalls, not four: the syscall total and the steal figure ride in `KernelStats`
    /// precisely so a sampler need not also fetch `SyscallStats` (several KiB of per-syscall
    /// arrays) and `SysInfo` (otherwise static) once a second.
    fn take() -> Self {
        let kernel = sys_kernel_stats();
        SysSample {
            syscalls: kernel.syscalls,
            steal_ns: kernel.steal_ns,
            kernel,
            mem: sys_memory_stats(),
            at: Instant::now(),
        }
    }
}

impl SysRates {
    /// Interrupts that were not the timer. Saturating: the two counters are read in one
    /// syscall but incremented independently, so a tick landing between them could otherwise
    /// produce a negative "other".
    fn other_irq(&self) -> f64 {
        (self.interrupts - self.hardticks).max(0.0)
    }
}

/// Per-second rates between two [SysSample]s, plus the levels that are read as-is.
#[derive(Default, Copy, Clone)]
struct SysRates {
    interrupts: f64,
    /// Scheduler timer ticks, a subset of `interrupts`.
    hardticks: f64,
    ctx_switches: f64,
    preempts: f64,
    faults: f64,
    syscalls: f64,
    /// Every invalidation. `shootdowns` counts only the ones that had to be sent to another cpu,
    /// so a single-threaded workload reads ~0 shootdowns against a busy flush count -- they are
    /// different questions and the header labels them separately for that reason.
    tlb_flushes: f64,
    tlb_shootdowns: f64,
    pager_requests: f64,
    pager_pages_in: f64,
    /// Fraction of one cpu's worth of time the hypervisor spent elsewhere while we were
    /// runnable, over the interval.
    steal: f64,
    /// Levels, not rates.
    pager_inflight: u64,
    pager_outstanding_frames: usize,
    free_bytes: usize,
    total_bytes: usize,
}

#[derive(Default)]
struct ThreadTracker {
    threads: BTreeMap<ObjID, ThreadInfo>,
    /// Statticks in the last interval, taken as the largest per-thread total delta. Every
    /// thread alive across the whole interval advances by exactly the elapsed time (the
    /// kernel charges the time a thread wasn't scheduled to its idle counter), so the max
    /// is that time, and a thread created part way through can only report less.
    elapsed: u64,
    ticks_per_sec: f64,
    last_sample: Option<Instant>,
    /// Compartment instance id -> name, `None` for one the monitor would not name for us.
    /// Cached across samples: each miss costs a gate call into the monitor, and both hits and
    /// misses are stable for the life of a compartment.
    comps: BTreeMap<ObjID, Option<String>>,
    view: ThreadView,
    /// Thread object ids are off by default: they are wide, and the name is what identifies a
    /// thread to a person. `i` brings them back for when an id is what you actually need.
    show_ids: bool,
    sort: SortKey,
    /// Inverts whatever the sort key's natural direction is.
    rev: bool,
    /// The previous and current system-wide samples, and the wall seconds between them. Wall
    /// time, not statticks: these count events, and a per-second rate is what a reader can
    /// compare against another machine.
    sys_prev: Option<SysSample>,
    sys_now: Option<SysSample>,
    secs: f64,
    /// Counts samples taken, for the periodic name refresh below.
    generation: u64,
    /// Read once. Hotplug aside, this does not change, and it was costing a `sys_info` every
    /// second to re-learn.
    cpus: usize,
    /// Our own thread id, for the bold self row. Constant for the life of the process.
    self_id: ObjID,
}

impl ThreadTracker {
    fn sample(&mut self) {
        let now = Instant::now();
        self.generation += 1;
        if self.cpus == 0 {
            self.cpus = sys_info().cpu_count;
            self.self_id = sys_thread_self_id();
        }
        self.sys_prev = self.sys_now;
        let sample = SysSample::take();
        self.secs = self
            .sys_prev
            .map(|p| sample.at.duration_since(p.at).as_secs_f64())
            .unwrap_or(0.0);
        self.sys_now = Some(sample);
        self.scan_for_threads();
        self.read_thread_stats();
        self.read_thread_names();
        self.read_thread_sctx();
        self.resolve_comp_names();

        self.elapsed = self
            .threads
            .values()
            .map(|t| total(&t.delta))
            .max()
            .unwrap_or(0);
        if let Some(last) = self.last_sample {
            let secs = now.duration_since(last).as_secs_f64();
            if secs > 0.0 && self.elapsed > 0 {
                self.ticks_per_sec = self.elapsed as f64 / secs;
            }
        }
        self.last_sample = Some(now);
    }

    fn scan_for_threads(&mut self) {
        for thread in self.threads.values_mut() {
            thread.seen = false;
        }

        let mut buf = [ObjID::default(); 128];
        let mut offset = 0;

        loop {
            match twizzler_abi::syscall::sys_enumerate(EnumerateKind::Threads, &mut buf, offset) {
                Ok(count) => {
                    if count == 0 {
                        break;
                    }

                    for id in &buf[0..count] {
                        self.threads
                            .entry(*id)
                            .and_modify(|t| t.seen = true)
                            .or_insert_with(|| ThreadInfo {
                                id: *id,
                                name: None,
                                state: None,
                                repr: None,
                                stats: ThreadSchedStats::default(),
                                delta: ThreadSchedStats::default(),
                                fresh: true,
                                err: None,
                                seen: true,
                                sctx: None,
                                kernel: false,
                            });
                    }

                    offset += count;
                }
                Err(_) => break,
            }
        }

        // A thread is only dropped when it leaves the kernel's thread list. A failed stat or
        // map is transient (or a permission problem) and must not silently empty the table.
        self.threads.retain(|_, t| t.seen);
    }

    fn read_thread_stats(&mut self) {
        for thread in self.threads.values_mut() {
            let mut stats = ThreadSchedStats::default();
            match sys_thread_read_stats(thread.id, &mut stats) {
                Ok(()) => {
                    thread.delta = if thread.fresh {
                        ThreadSchedStats::default()
                    } else {
                        ThreadSchedStats {
                            user: stats.user.saturating_sub(thread.stats.user),
                            system: stats.system.saturating_sub(thread.stats.system),
                            idle: stats.idle.saturating_sub(thread.stats.idle),
                            faults: stats.faults.saturating_sub(thread.stats.faults),
                            pager_pages: stats.pager_pages.saturating_sub(thread.stats.pager_pages),
                            syscalls: stats.syscalls.saturating_sub(thread.stats.syscalls),
                            wakes: stats.wakes.saturating_sub(thread.stats.wakes),
                        }
                    };
                    thread.stats = stats;
                    thread.fresh = false;
                    thread.err = None;
                }
                Err(e) => {
                    thread.delta = ThreadSchedStats::default();
                    thread.err = Some(e);
                }
            }

            if thread.repr.is_none() {
                thread.repr = Object::<ThreadRepr>::map(thread.id, MapFlags::READ).ok();
            }
            thread.state = thread.repr.as_ref().map(|repr| repr.base().get_state());
            thread.kernel = thread
                .repr
                .as_ref()
                .is_some_and(|repr| repr.base().is_kernel());
        }
    }

    /// Names cost two syscalls each (enumerate the notes, read the last one) and almost never
    /// change, so they are read once per thread and then only occasionally. A thread with no name
    /// yet is retried every sample: the runtime sets it shortly after spawn, and a permanently
    /// blank row would be the worse failure.
    const NAME_REFRESH: u64 = 32;

    fn read_thread_names(&mut self) {
        let refresh = self.generation % Self::NAME_REFRESH == 0;
        for thread in self.threads.values_mut() {
            if thread.name.is_some() && !refresh {
                continue;
            }
            if let Some(name) = try_read_thread_name(thread.id) {
                thread.name = Some(name);
            }
        }
    }

    fn read_thread_sctx(&mut self) {
        for thread in self.threads.values_mut() {
            // A kernel thread belongs to no security context and cannot be in a gate call, so
            // both ids are zero by construction -- which is what the syscall would tell us, at
            // the cost of a syscall each per second.
            if thread.kernel {
                thread.sctx = Some(ThreadSctxIds::default());
                continue;
            }
            let mut ids = ThreadSctxIds::default();
            thread.sctx = sys_thread_read_sctx_ids(thread.id, &mut ids)
                .ok()
                .map(|()| ids);
        }
    }

    fn resolve_comp_names(&mut self) {
        let mut wanted: Vec<ObjID> = Vec::new();
        for thread in self.threads.values() {
            let Some(sctx) = thread.sctx else { continue };
            for id in [sctx.home, sctx.active] {
                if id.raw() != 0 && !self.comps.contains_key(&id) && !wanted.contains(&id) {
                    wanted.push(id);
                }
            }
        }
        for id in wanted {
            let name = CompartmentHandle::lookup_id(id)
                .ok()
                .and_then(|handle| handle.info().ok().map(|info| info.name));
            self.comps.insert(id, name);
        }
    }

    /// How to label a security context in the display.
    fn comp_label(&self, id: ObjID) -> String {
        if id.raw() == 0 {
            // The monitor's own instance id *is* zero (`MONITOR_INSTANCE_ID`), so this is where
            // its threads land. It also catches the two other zero-home userspace populations,
            // bootstrap and statically-linked programs, which are absent from a normal boot --
            // nothing in the pair of context ids separates them from the monitor.
            return "[monitor]".to_string();
        }
        match self.comps.get(&id) {
            // Basename only. Compartments are named by path, and at a normal terminal width the
            // shared "/pkg/" prefix is all that survives truncation -- two different compartments
            // both rendering as "/pkg/twi", which is worse than useless.
            Some(Some(name)) => {
                let base = name.rsplit('/').next().unwrap_or("");
                if base.is_empty() {
                    name.clone()
                } else {
                    base.to_string()
                }
            }
            // Known-unnamed and never-seen look the same here on purpose: either way the id is
            // all we have to show.
            _ => format!("[{:x}]", id),
        }
    }

    /// The group a thread is displayed under. A zero home means no compartment, which covers
    /// both kernel threads and the userspace ones that belong to none -- the monitor's own,
    /// bootstrap, and statically-linked programs. Only the repr flag separates those.
    fn group_label(&self, thread: &ThreadInfo) -> String {
        if thread.home().raw() == 0 && thread.kernel {
            return "[kernel]".to_string();
        }
        self.comp_label(thread.home())
    }

    /// Threads grouped by home compartment, hottest group first, hottest thread first inside it.
    fn grouped(&self) -> Vec<CompGroup<'_>> {
        let mut by_home: BTreeMap<String, Vec<&ThreadInfo>> = BTreeMap::new();
        for thread in self.sorted() {
            by_home
                .entry(self.group_label(thread))
                .or_default()
                .push(thread);
        }
        let mut groups: Vec<CompGroup<'_>> = by_home
            .into_iter()
            .map(|(label, threads)| CompGroup {
                ticks: threads.iter().map(|t| t.delta_cpu_ticks()).sum(),
                user_ticks: threads.iter().map(|t| t.delta.user).sum(),
                sys_ticks: threads.iter().map(|t| t.delta.system).sum(),
                fault_count: threads.iter().map(|t| t.delta.faults).sum(),
                pager_count: threads.iter().map(|t| t.delta.pager_pages).sum(),
                syscall_count: threads.iter().map(|t| t.delta.syscalls).sum(),
                wake_count: threads.iter().map(|t| t.delta.wakes).sum(),
                cross: threads.iter().filter(|t| t.is_cross()).count(),
                // Every thread in a group shares a home by construction, so the first answers
                // for all of them.
                system: threads.first().is_some_and(|t| t.home().raw() == 0),
                label,
                threads,
            })
            .collect();
        groups.sort_by(|a, b| {
            // Id has no group-level analogue, so it falls back to the label like Name does.
            let ord = match self.sort {
                SortKey::Cpu => b.ticks.cmp(&a.ticks),
                SortKey::User => b.user_ticks.cmp(&a.user_ticks),
                SortKey::Sys => b.sys_ticks.cmp(&a.sys_ticks),
                SortKey::Faults => b.fault_count.cmp(&a.fault_count),
                SortKey::Pager => b.pager_count.cmp(&a.pager_count),
                SortKey::Syscalls => b.syscall_count.cmp(&a.syscall_count),
                SortKey::Wakes => b.wake_count.cmp(&a.wake_count),
                SortKey::Time => b.cpu_ticks().cmp(&a.cpu_ticks()),
                SortKey::State => b.running().cmp(&a.running()),
                SortKey::Name | SortKey::Id => a.label.cmp(&b.label),
            };
            let ord = if self.rev { ord.reverse() } else { ord };
            ord.then(b.threads.len().cmp(&a.threads.len()))
                .then(a.label.cmp(&b.label))
        });
        groups
    }

    /// Whether this group's individual thread rows are drawn under the current mode.
    fn show_rows(&self, group: &CompGroup<'_>) -> bool {
        match self.view {
            ThreadView::All => true,
            ThreadView::HideSystem => !group.system,
            ThreadView::HideAll => false,
        }
    }

    /// Where a thread is executing, when that is not its home compartment.
    fn away_label(&self, thread: &ThreadInfo) -> String {
        if !thread.is_cross() {
            return String::new();
        }
        let active = thread.sctx.unwrap().active;
        // A cross thread has a nonzero home by definition, so a zero *active* is not the
        // "belongs to nothing" case the group label describes -- it is a gate call into the
        // monitor, whose instance id is zero.
        if active.raw() == 0 {
            return "-> [monitor]".to_string();
        }
        format!("-> {}", self.comp_label(active))
    }

    /// Threads in the current sort order.
    fn sorted(&self) -> Vec<&ThreadInfo> {
        let mut visible: Vec<&ThreadInfo> = self.threads.values().collect();
        visible.sort_by(|a, b| {
            let ord = match self.sort {
                SortKey::Cpu => b
                    .delta_cpu_ticks()
                    .cmp(&a.delta_cpu_ticks())
                    .then(b.cpu_ticks().cmp(&a.cpu_ticks())),
                SortKey::User => b.delta.user.cmp(&a.delta.user),
                SortKey::Sys => b.delta.system.cmp(&a.delta.system),
                SortKey::Faults => b.delta.faults.cmp(&a.delta.faults),
                SortKey::Pager => b.delta.pager_pages.cmp(&a.delta.pager_pages),
                SortKey::Syscalls => b.delta.syscalls.cmp(&a.delta.syscalls),
                SortKey::Wakes => b.delta.wakes.cmp(&a.delta.wakes),
                SortKey::Time => b.cpu_ticks().cmp(&a.cpu_ticks()),
                // Sorts on what is drawn, so an unnamed thread orders by the id shown in its
                // place rather than sinking to one end as an empty string.
                SortKey::Name => display_name(a).cmp(&display_name(b)),
                SortKey::State => state_str(a).cmp(state_str(b)),
                SortKey::Id => a.id.cmp(&b.id),
            };
            let ord = if self.rev { ord.reverse() } else { ord };
            ord.then(a.id.cmp(&b.id))
        });
        visible
    }

    /// The direction actually in effect, for the footer.
    fn sort_dir(&self) -> &'static str {
        if self.sort.descends() != self.rev {
            "desc"
        } else {
            "asc"
        }
    }

    /// System-wide rates for the header. Zero everywhere until the second sample: one sample has
    /// nothing to difference against, and showing a since-boot total in a per-second column would
    /// read as a rate.
    fn rates(&self) -> SysRates {
        let Some(now) = self.sys_now else {
            return SysRates::default();
        };
        let mut r = SysRates {
            pager_inflight: now.kernel.pager_inflight,
            pager_outstanding_frames: now.mem.tracker.pager_outstanding,
            free_bytes: now.mem.free_bytes(),
            total_bytes: now.mem.total_bytes(),
            ..Default::default()
        };
        let (Some(prev), true) = (self.sys_prev, self.secs > 0.0) else {
            return r;
        };
        let per = |a: u64, b: u64| rate(a.saturating_sub(b), self.secs);
        r.interrupts = per(now.kernel.interrupts, prev.kernel.interrupts);
        r.hardticks = per(now.kernel.hardticks, prev.kernel.hardticks);
        r.ctx_switches = per(now.kernel.ctx_switches, prev.kernel.ctx_switches);
        r.preempts = per(now.kernel.preempts, prev.kernel.preempts);
        r.syscalls = per(now.syscalls, prev.syscalls);
        r.pager_requests = per(now.kernel.pager_requests, prev.kernel.pager_requests);
        r.pager_pages_in = per(
            now.kernel.pager_pages_installed,
            prev.kernel.pager_pages_installed,
        );
        r.faults = per(
            now.mem.page_fault_count as u64,
            prev.mem.page_fault_count as u64,
        );
        r.steal = now.steal_ns.saturating_sub(prev.steal_ns) as f64 / (self.secs * 1e9);
        r.tlb_flushes = per(
            now.mem.tlb_flush_count as u64,
            prev.mem.tlb_flush_count as u64,
        );
        r.tlb_shootdowns = per(
            now.mem.tlb_shootdown_count as u64,
            prev.mem.tlb_shootdown_count as u64,
        );
        r
    }

    /// A rate for display. Before the second sample there is no interval to divide by, and
    /// "no data yet" must not look like "zero" -- so that case, and only that case, prints "-".
    fn rate_str(&self, v: f64) -> String {
        if self.secs <= 0.0 {
            "-".to_string()
        } else {
            fmt_rate(v)
        }
    }

    /// The two system-wide header lines, as text.
    fn sys_lines(&self) -> (String, String) {
        let r = self.rates();
        // Timer and everything else, side by side rather than one total. `timer` is the hardtick
        // cadence -- the one-shot is rearmed at most `NANOS_PER_TICK` (1ms) out, so ~1000/s is the
        // designed rate and not evidence of anything. It is a different clock from the statclock
        // rate in the summary line, which samples thread time at ~125/s; both used to be called
        // "ticks", which invited exactly the wrong conclusion.
        let kernel = format!(
            "kernel  timer {}/s  otherirq {}/s  ctxsw {}/s  preempt {}/s  fault {}/s  syscall {}/s  tlbflush {}/s  shootdown {}/s",
            self.rate_str(r.hardticks),
            self.rate_str(r.other_irq()),
            self.rate_str(r.ctx_switches),
            self.rate_str(r.preempts),
            self.rate_str(r.faults),
            self.rate_str(r.syscalls),
            self.rate_str(r.tlb_flushes),
            self.rate_str(r.tlb_shootdowns),
        );
        // Only when the hypervisor is actually taking time: on bare metal it is always zero, and
        // a permanent "steal 0.0" column trains the eye to skip the spot where it matters.
        let kernel = if r.steal > 0.0 {
            format!("{}  steal {:.2} cpu", kernel, r.steal)
        } else {
            kernel
        };
        let pager = format!(
            "pager   {}/s req  {}/s pages in  {} in flight  {} frames on loan  |  mem {} free of {}",
            self.rate_str(r.pager_requests),
            self.rate_str(r.pager_pages_in),
            r.pager_inflight,
            r.pager_outstanding_frames,
            fmt_bytes(r.free_bytes),
            fmt_bytes(r.total_bytes),
        );
        (kernel, pager)
    }

    fn summary(&self) -> String {
        let stats = sys_thread_stats();
        let cpus = self.cpus;
        let busy: f64 = self.threads.values().map(|t| t.cpu(self.elapsed)).sum();
        let groups = self.grouped();
        let cross: usize = groups.iter().map(|g| g.cross).sum();
        format!(
            "  —  {} threads ({} running, {} blocked) in {} compartments, {} cross, {:.2}/{} cpus busy, statclock {}/s",
            stats.nr_threads,
            stats.nr_running,
            stats.nr_blocked,
            groups.len(),
            cross,
            busy,
            cpus,
            if self.ticks_per_sec > 0.0 {
                format!("{:.0}", self.ticks_per_sec)
            } else {
                "?".to_string()
            }
        )
    }

    fn time_str(&self, thread: &ThreadInfo) -> String {
        self.time_str_ticks(thread.cpu_ticks())
    }

    fn time_str_ticks(&self, ticks: u64) -> String {
        if self.ticks_per_sec <= 0.0 {
            return "-".to_string();
        }
        fmt_time(ticks as f64 / self.ticks_per_sec)
    }

    fn render_plain(&self, out: &mut impl Write) -> std::io::Result<()> {
        writeln!(out, "twiztop{}", self.summary())?;
        let (kernel_line, pager_line) = self.sys_lines();
        writeln!(out, "{}", kernel_line)?;
        writeln!(out, "{}", pager_line)?;
        let id_hdr = if self.show_ids {
            format!("{:<20}  ", "ID")
        } else {
            String::new()
        };
        writeln!(
            out,
            "{}{:<22}  {:<18}  {:<9}  {:>6}  {:>6}  {:>6}  {:>6}  {:>6}  {:>6}  {:>6}  {:>8}  {}",
            id_hdr,
            "NAME",
            "COMP",
            "STATE",
            "CPU%",
            "USER%",
            "SYS%",
            "FLT/s",
            "PGR/s",
            "SC/s",
            "WAKE/s",
            "TIME",
            "WHERE"
        )?;
        // Flat and one-compartment-per-row rather than grouped: this output is meant to be
        // parsed, and a grouped form would put the compartment on a different line than the
        // thread it describes.
        for group in self.grouped() {
            // Collapsed groups still emit one line, so a parser sees every compartment and its
            // cost whichever mode produced the output.
            if !self.show_rows(&group) {
                writeln!(
                    out,
                    "{}{:<22.22}  {:<18.18}  {:<9.9}  {:>5.1}%  {:>5.1}%  {:>5.1}%  {:>6}  {:>6}  {:>6}  {:>6}  {:>8}  {}",
                    if self.show_ids {
                        format!("{:<20}  ", "")
                    } else {
                        String::new()
                    },
                    "",
                    group.label,
                    format!("{} run", group.running()),
                    group.cpu(self.elapsed) * 100.0,
                    group.user(self.elapsed) * 100.0,
                    group.system(self.elapsed) * 100.0,
                    self.rate_str(group.faults(self.secs)),
                    self.rate_str(group.pager(self.secs)),
                    self.rate_str(group.syscalls(self.secs)),
                    self.rate_str(group.wakes(self.secs)),
                    self.time_str_ticks(group.cpu_ticks()),
                    group.tail(),
                )?;
                continue;
            }
            for thread in &group.threads {
                writeln!(
                    out,
                    "{}{:<22.22}  {:<18.18}  {:<9.9}  {:>5.1}%  {:>5.1}%  {:>5.1}%  {:>6}  {:>6}  {:>6}  {:>6}  {:>8}  {}",
                    if self.show_ids {
                        format!("{:<20.20}  ", format!("{:x}", thread.id))
                    } else {
                        String::new()
                    },
                    display_name(thread),
                    group.label,
                    state_str(thread),
                    thread.cpu(self.elapsed) * 100.0,
                    thread.user(self.elapsed) * 100.0,
                    thread.system(self.elapsed) * 100.0,
                    self.rate_str(thread.faults(self.secs)),
                    self.rate_str(thread.pager(self.secs)),
                    self.rate_str(thread.syscalls(self.secs)),
                    self.rate_str(thread.wakes(self.secs)),
                    self.time_str(thread),
                    self.away_label(thread),
                )?;
            }
        }
        out.flush()
    }

    fn render(&self, out: &mut impl Write) -> std::io::Result<()> {
        let (cols, rows) = size().unwrap_or((80, 24));
        let self_id = self.self_id;
        // Everything but NAME is fixed width; NAME takes what is left. Thread rows are indented
        // two under their compartment header, and WHERE takes a column at the end, so both come
        // out of NAME's share. Hiding the ID column hands its 18 columns to NAME.
        //
        // 2 indent + 18 id (when shown) + 2 + 9 state + six 8-wide numeric columns + 10 time +
        // 2 before WHERE, and ~10 reserved for WHERE itself.
        let id_w = if self.show_ids { 18 } else { 0 };
        // USER%/SYS% are a breakdown of CPU%, so they are what a narrow terminal can afford to
        // lose: the alternative is starving NAME until every compartment reads as its shared path
        // prefix. Their 16 columns go back to NAME below this width.
        let split_cpu = cols as usize >= 118 + id_w;
        let fixed = if split_cpu { 81 } else { 65 };
        let name_w = (cols as usize)
            .saturating_sub(fixed + id_w + 10)
            .clamp(6, 40);
        let id_hdr = if self.show_ids {
            format!("{:<16}  ", "ID")
        } else {
            String::new()
        };

        let visible = self.sorted();
        let mut rows_by_state: BTreeMap<&'static str, usize> = BTreeMap::new();
        for thread in &visible {
            *rows_by_state.entry(state_str(thread)).or_default() += 1;
        }

        out.queue(Clear(ClearType::All))?;
        out.queue(MoveTo(0, 0))?;

        out.queue(SetAttribute(Attribute::Bold))?;
        out.queue(SetForegroundColor(Color::Cyan))?;
        out.queue(Print("twiztop"))?;
        out.queue(ResetColor)?;
        out.queue(Print(self.summary()))?;

        out.queue(MoveTo(0, 1))?;
        let mut first = true;
        for (state, count) in &rows_by_state {
            if !first {
                out.queue(Print("  "))?;
            }
            first = false;
            out.queue(SetForegroundColor(state_color(state)))?;
            out.queue(Print(format!("{}: {}", state, count)))?;
            out.queue(ResetColor)?;
        }

        let (kernel_line, pager_line) = self.sys_lines();
        out.queue(MoveTo(0, 2))?;
        out.queue(SetForegroundColor(Color::DarkGrey))?;
        out.queue(Print(&kernel_line))?;
        out.queue(MoveTo(0, 3))?;
        out.queue(Print(&pager_line))?;
        out.queue(ResetColor)?;

        out.queue(MoveTo(0, 4))?;
        out.queue(SetAttribute(Attribute::Reverse))?;
        out.queue(Print(pad(
            &format!(
                "  {}{:<w$}  {:<9}  {:>6}{}  {:>6}  {:>6}  {:>6}  {:>6}  {:>8}  {}",
                id_hdr,
                "NAME",
                "STATE",
                "CPU%",
                if split_cpu {
                    format!("  {:>6}  {:>6}", "USER%", "SYS%")
                } else {
                    String::new()
                },
                "FLT/s",
                "PGR/s",
                "SC/s",
                "WAKE/s",
                "TIME",
                "WHERE",
                w = name_w
            ),
            cols as usize,
        )))?;
        out.queue(SetAttribute(Attribute::Reset))?;

        let body_start: u16 = 5;
        let max_rows = rows.saturating_sub(body_start + 1) as usize;
        // A group costs a header row, so budget rows across groups rather than threads: an
        // overlong first group must not push every other compartment off the screen entirely.
        let mut row = 0usize;
        for group in self.grouped() {
            let show_rows = self.show_rows(&group);
            // A collapsed group needs only its own line; an expanded one is not worth starting
            // with no room for a single thread under it.
            if row + if show_rows { 2 } else { 1 } > max_rows {
                break;
            }
            out.queue(MoveTo(0, body_start + row as u16))?;
            // The label spans the ID and NAME columns, so the numeric columns below line up
            // with the thread rows rather than sitting wherever the label happened to end.
            let label_w = if self.show_ids {
                20 + name_w
            } else {
                2 + name_w
            };
            out.queue(SetAttribute(Attribute::Bold))?;
            out.queue(SetForegroundColor(Color::Magenta))?;
            out.queue(Print(format!("{:<w$.w$}", group.label, w = label_w)))?;
            out.queue(ResetColor)?;
            out.queue(SetAttribute(Attribute::Reset))?;
            out.queue(Print("  "))?;
            // Running count in the STATE column: the one per-compartment fact the rows below
            // can no longer supply once they are hidden.
            let running = group.running();
            out.queue(SetForegroundColor(if running > 0 {
                Color::Green
            } else {
                Color::DarkGrey
            }))?;
            out.queue(Print(format!("{:<9.9}", format!("{} run", running))))?;
            out.queue(ResetColor)?;

            let group_fracs: Vec<f64> = if split_cpu {
                vec![
                    group.cpu(self.elapsed),
                    group.user(self.elapsed),
                    group.system(self.elapsed),
                ]
            } else {
                vec![group.cpu(self.elapsed)]
            };
            for frac in group_fracs {
                out.queue(Print("  "))?;
                out.queue(SetForegroundColor(pct_color(frac)))?;
                out.queue(Print(format!("{:>5.1}%", frac * 100.0)))?;
                out.queue(ResetColor)?;
            }
            for rate in [
                group.faults(self.secs),
                group.pager(self.secs),
                group.syscalls(self.secs),
                group.wakes(self.secs),
            ] {
                out.queue(Print("  "))?;
                out.queue(SetForegroundColor(rate_color(rate)))?;
                out.queue(Print(format!("{:>6}", self.rate_str(rate))))?;
                out.queue(ResetColor)?;
            }
            out.queue(Print(format!(
                "  {:>8}  ",
                self.time_str_ticks(group.cpu_ticks())
            )))?;
            out.queue(SetForegroundColor(if group.cross > 0 {
                Color::Yellow
            } else {
                Color::DarkGrey
            }))?;
            out.queue(Print(group.tail()))?;
            out.queue(ResetColor)?;
            row += 1;

            if !show_rows {
                continue;
            }

            for thread in &group.threads {
                if row >= max_rows {
                    break;
                }
                out.queue(MoveTo(0, body_start + row as u16))?;
                row += 1;

                if thread.id == self_id {
                    out.queue(SetAttribute(Attribute::Bold))?;
                }
                out.queue(Print(format!(
                    "  {}{:<w$.w$}  ",
                    if self.show_ids {
                        format!("{:<16.16}  ", format!("{:x}", thread.id))
                    } else {
                        String::new()
                    },
                    display_name(thread),
                    w = name_w
                )))?;
                out.queue(SetForegroundColor(state_color(state_str(thread))))?;
                out.queue(Print(format!("{:<9.9}", state_str(thread))))?;
                out.queue(ResetColor)?;

                let fracs: Vec<f64> = if split_cpu {
                    vec![
                        thread.cpu(self.elapsed),
                        thread.user(self.elapsed),
                        thread.system(self.elapsed),
                    ]
                } else {
                    vec![thread.cpu(self.elapsed)]
                };
                for frac in fracs {
                    out.queue(Print("  "))?;
                    out.queue(SetForegroundColor(pct_color(frac)))?;
                    out.queue(Print(format!("{:>5.1}%", frac * 100.0)))?;
                    out.queue(ResetColor)?;
                }
                for rate in [
                    thread.faults(self.secs),
                    thread.pager(self.secs),
                    thread.syscalls(self.secs),
                    thread.wakes(self.secs),
                ] {
                    out.queue(Print("  "))?;
                    out.queue(SetForegroundColor(rate_color(rate)))?;
                    out.queue(Print(format!("{:>6}", self.rate_str(rate))))?;
                    out.queue(ResetColor)?;
                }
                out.queue(Print(format!("  {:>8}  ", self.time_str(thread))))?;
                if thread.is_cross() {
                    out.queue(SetForegroundColor(Color::Yellow))?;
                    out.queue(Print(self.away_label(thread)))?;
                    out.queue(ResetColor)?;
                }
                out.queue(SetAttribute(Attribute::Reset))?;
            }
        }

        out.queue(MoveTo(0, rows.saturating_sub(1)))?;
        out.queue(SetForegroundColor(Color::DarkGrey))?;
        out.queue(Print(format!(
            "q/Esc: quit  |  t: {}  |  s: sort by {} ({})  |  r: reverse  |  i: {} ids  |  grouped by home compartment",
            self.view.label(),
            self.sort.label(),
            self.sort_dir(),
            if self.show_ids { "hide" } else { "show" },
        )))?;
        out.queue(ResetColor)?;

        out.flush()
    }
}

/// Thread names are mirrored into notes on the thread's repr object (see the reference
/// runtime's `set_name`), so that is the only place to read one from. The most recently
/// added note is the most specific label the thread has given itself.
fn try_read_thread_name(id: ObjID) -> Option<String> {
    let mut last_key = None;
    let mut offset = 0;
    loop {
        let mut keys = [0u64; 16];
        let n = sys_object_enumerate_notes(id, offset, &mut keys).ok()?;
        if n == 0 {
            break;
        }
        last_key = Some(keys[n - 1]);
        offset += n;
        if n < keys.len() {
            break;
        }
    }

    let mut buf = [0u8; 128];
    let len = sys_object_get_note(id, last_key?, &mut buf).ok()?;
    let s = std::str::from_utf8(&buf[..len.min(buf.len())]).ok()?;
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// What goes in the NAME column. An unnamed thread (kernel threads, and anything that never
/// called set_name) still needs an identity there, since the id column is off by default.
fn display_name(thread: &ThreadInfo) -> String {
    match thread.name.as_deref() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            // Low bits only: ids are wide, and the low half is what distinguishes them.
            format!("<{:08x}>", thread.id.raw() as u32)
        }
    }
}

fn state_str(thread: &ThreadInfo) -> &'static str {
    if thread.err.is_some() {
        return "?";
    }
    match thread.state {
        Some(ExecutionState::Running) => "Running",
        Some(ExecutionState::Sleeping) => "Sleeping",
        Some(ExecutionState::Suspended) => "Suspended",
        Some(ExecutionState::Exited) => "Exited",
        None => "?",
    }
}

fn state_color(state: &str) -> Color {
    match state {
        "Running" => Color::Green,
        "Sleeping" => Color::Blue,
        "Exited" => Color::DarkGrey,
        "?" => Color::DarkGrey,
        _ => Color::Yellow,
    }
}

/// Rates have no ceiling to scale against the way a cpu percentage does, so this only separates
/// "doing nothing" from "doing something" and flags the genuinely loud.
fn rate_color(v: f64) -> Color {
    match v {
        v if v >= 10_000.0 => Color::Red,
        v if v >= 1_000.0 => Color::Yellow,
        v if v > 0.0 => Color::Green,
        _ => Color::DarkGrey,
    }
}

fn pct_color(frac: f64) -> Color {
    match frac {
        f if f >= 0.75 => Color::Red,
        f if f >= 0.40 => Color::Yellow,
        f if f > 0.0 => Color::Green,
        _ => Color::DarkGrey,
    }
}

fn fmt_bytes(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1}{}", v, UNITS[i])
}

fn fmt_time(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0:00.0".to_string();
    }
    let mins = (secs / 60.0).floor();
    format!("{}:{:04.1}", mins as u64, secs - mins * 60.0)
}

fn pad(s: &str, width: usize) -> String {
    let mut s = s.to_string();
    if s.len() < width {
        s.push_str(&" ".repeat(width - s.len()));
    }
    s
}
