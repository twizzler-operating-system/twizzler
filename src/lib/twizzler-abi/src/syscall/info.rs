use core::num::NonZeroUsize;

use twizzler_rt_abi::error::TwzError;

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{arch::syscall::raw_syscall, syscall::TimeSpan};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum InfoKind {
    SysInfo = 0,
    MemoryStats = 1,
    ThreadStats = 2,
    SctxStats = 3,
    LockStats = 4,
    SyscallStats = 5,
    ObjectStats = 6,
    KernelStats = 9,
    /// One cpu's topology, caches and frequency; the third syscall argument selects which. See
    /// [sys_cpu_info].
    CpuInfo = 10,
}

impl TryFrom<u64> for InfoKind {
    type Error = TwzError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(InfoKind::SysInfo),
            1 => Ok(InfoKind::MemoryStats),
            2 => Ok(InfoKind::ThreadStats),
            3 => Ok(InfoKind::SctxStats),
            4 => Ok(InfoKind::LockStats),
            5 => Ok(InfoKind::SyscallStats),
            6 => Ok(InfoKind::ObjectStats),
            9 => Ok(InfoKind::KernelStats),
            10 => Ok(InfoKind::CpuInfo),
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }
}

impl From<InfoKind> for u64 {
    fn from(val: InfoKind) -> Self {
        val as u64
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Information about the system.
pub struct SysInfo {
    /// The version of this data structure, to allow expansion.
    pub version: u32,
    /// Flags. Currently unused.
    pub flags: u32,
    /// The number of CPUs on this system. Hyperthreads are counted as individual CPUs.
    pub cpu_count: usize,
    /// The size of a virtual address page on this system.
    pub page_size: usize,
    /// Cumulative nanoseconds the hypervisor ran something else while this system's cpus were
    /// runnable, summed across cpus (KVM steal time). 0 on bare metal or when the hypervisor
    /// does not report it. Nonzero-and-growing during a measurement means the numbers were taken
    /// on a contended host. Present from `version` 2.
    pub steal_ns: u64,
}

impl SysInfo {
    /// Get the number of CPUs on the system.
    pub fn cpu_count(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.cpu_count).expect("CPU count from sysinfo should always be non-zero")
    }

    /// Get the page size of the system.
    pub fn page_size(&self) -> usize {
        self.page_size
    }
}

#[derive(Debug, Copy, Clone, Default)]
#[repr(C)]
pub struct MemoryStats {
    pub version: u32,
    pub flags: u32,
    pub nr_levels: usize,
    pub total_pages: usize,
    pub levels: [MemoryStatsLevel; 8],
    pub late_kalloc_bytes: usize,
    pub early_kalloc_bytes: usize,
    pub tlb_shootdown_count: usize,
    pub tlb_flush_count: usize,
    pub page_fault_count: usize,
    pub page_fault_stats: TimeStat,
    /// Address-space switches that reloaded the page-table root and flushed.
    pub aspace_switch_flush_count: usize,
    /// Address-space switches that reloaded the root without flushing, which is the whole point of
    /// PCIDs (x86_64) -- against `aspace_switch_flush_count` this is the fraction of switches that
    /// used to flush and no longer do. Zero on hardware or builds without them.
    pub aspace_switch_noflush_count: usize,
    /// Times a processor's right to take that no-flush path was revoked by an invalidation on
    /// another processor. Read against the two above: this is what eats the saving.
    pub tlb_revoke_count: usize,
    /// Frame-tracker state, in frames. `free_pages` above is the physical allocator's view; these
    /// are the tracker's, and the two answer different questions. A frame parked in a thread-local
    /// precharge pool is neither free nor mapped -- it is counted here under `kernel_used` and is
    /// absent from `free_pages`, which is how 175k of them once hid in one thread's pool.
    pub tracker: TrackerStats,
}

/// The frame tracker's counters, in frames. Invariant the tracker intends to hold:
/// `idle + kernel_used + page_data == total`, with `pager_outstanding` a subset of `page_data`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[repr(C)]
pub struct TrackerStats {
    /// Frames on the allocator's free lists.
    pub idle: usize,
    /// Frames charged to the kernel (page tables, kernel heap backing, precharge pools).
    pub kernel_used: usize,
    /// Frames holding object page data.
    pub page_data: usize,
    /// Every frame the tracker knows about.
    pub total: usize,
    /// Frames currently loaned to the userspace pager. A subset of `page_data`.
    pub pager_outstanding: usize,
    /// Cumulative frames allocated since boot. Monotone.
    pub allocated: usize,
    /// Cumulative frames freed since boot. Monotone.
    pub freed: usize,
    /// Cumulative frames recovered by the reclaim thread. Monotone.
    pub reclaimed: usize,
    /// Threads currently blocked waiting for a frame.
    pub waiting: usize,
    /// Whether the reclaim heuristic is currently latched on.
    pub reclaiming: bool,
    /// Frames sitting in a per-cpu frame cache rather than in live use.
    ///
    /// A subset of `kernel_used + page_data`: a cached frame stays `ALLOCATED` and stays charged
    /// to whatever class its last owner had, because moving the charge at cache-entry would make
    /// parking look like a systematic drain of `page_data` into `kernel_used`. That makes cache
    /// occupancy and a genuine leak *indistinguishable* in either class counter on its own, and in
    /// `idle`/`free_pages` too -- a cached frame has left the allocator's free list exactly as a
    /// leaked one has.
    ///
    /// So this exists to be subtracted. `kernel_used + page_data - pooled` is the live charged
    /// population and is the quantity a leak fit should be run against. Appended to the struct
    /// rather than inserted for the usual `repr(C)` reason.
    pub pooled: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Default)]
#[repr(C)]
pub struct MemoryStatsLevel {
    pub page_size: usize,
    pub free_pages: usize,
    pub lent_pages: usize,
    pub reserved_pages: usize,
}

impl MemoryStats {
    pub fn levels(&self) -> &[MemoryStatsLevel] {
        &self.levels[..self.nr_levels]
    }

    pub fn total_bytes(&self) -> usize {
        self.total_pages * self.levels[0].page_size
    }

    pub fn free_bytes(&self) -> usize {
        self.levels()
            .iter()
            .map(|l| l.page_size * l.free_pages)
            .sum()
    }

    pub fn lent_bytes(&self) -> usize {
        self.levels()
            .iter()
            .map(|l| l.page_size * l.lent_pages)
            .sum()
    }

    pub fn reserved_bytes(&self) -> usize {
        self.levels()
            .iter()
            .map(|l| l.page_size * l.reserved_pages)
            .sum()
    }

    pub fn kalloc_bytes(&self) -> usize {
        self.late_kalloc_bytes + self.early_kalloc_bytes
    }

    pub fn early_kalloc_bytes(&self) -> usize {
        self.early_kalloc_bytes
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct ThreadStats {
    pub nr_threads: usize,
    pub nr_running: usize,
    pub nr_blocked: usize,
    pub nr_pending_exit: usize,
    /// Threads that have exited and are waiting on a processor's cleanup list for their last
    /// reference to be dropped.
    ///
    /// Invisible to `nr_pending_exit`, which counts the registry: `exit` removes a thread from
    /// `ALL_THREADS` *before* it is pushed here, so a thread waiting to be reaped is in neither.
    /// Each one holds a 2 MiB kernel stack and its whole `Thread` allocation.
    pub nr_exited_backlog: usize,
    /// Threads reaped since boot. A backlog that is not falling while this is flat means reaping
    /// has stopped, not that it is merely slow.
    pub nr_reaped: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct SctxStats {
    pub nr_sctx: usize,
    pub nr_active: usize,
    pub nr_cached: usize,
}

#[derive(Debug, Copy, Clone, Default)]
#[repr(C)]
pub struct SyscallStats {
    pub nr_syscalls: usize,
    pub nr_syscalls_per_type: [usize; Syscall::NumSyscalls as usize],
    pub syscall_times: [TimeStat; Syscall::NumSyscalls as usize],
}

#[derive(Debug, Copy, Clone, Default)]
#[repr(C)]
pub struct TimeStat {
    pub mean: TimeSpan,
    pub running_mean: TimeSpan,
    pub min: TimeSpan,
    pub max: TimeSpan,
    pub variance: TimeSpan,
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub struct LockStats {
    pub mutex_lock_count: usize,
    pub mutex_waiting_count: usize,
    pub mutex_avg_waiting_time: TimeStat,
    pub mutex_hold_time: TimeStat,
}

/// Kernel activity counters that have no home in the memory/thread/object stats: interrupts,
/// scheduling, and the kernel's side of the pager conversation.
///
/// Every field is a since-boot total (`Cumulative`, in the vocabulary
/// `src/test/leakcheck/src/sample.rs` uses) except [KernelStats::pager_inflight], which is a
/// level. Rates are the reader's job: sample twice and divide by the wall time between.
#[derive(Debug, Copy, Clone, Default)]
#[repr(C)]
pub struct KernelStats {
    pub version: u32,
    pub flags: u32,
    /// Hardware interrupts dispatched, summed over cpus. Device interrupts, IPIs and the timer;
    /// cpu exceptions are not interrupts and are not counted here -- page faults have their own
    /// counter in [MemoryStats::page_fault_count].
    pub interrupts: u64,
    /// Scheduler timer ticks, a subset of `interrupts`.
    pub hardticks: u64,
    /// Thread context switches.
    pub ctx_switches: u64,
    /// Involuntary preemptions, a subset of `ctx_switches`.
    pub preempts: u64,
    /// Threads made runnable -- plus, on the current kernel, reschedule requests:
    /// `schedule_resched` charges those here too, so this exceeds the wake count by roughly
    /// `preempts`. Use [`KernelStats::thread_wakes`] for wakes alone.
    pub wakeups: u64,
    /// Threads made runnable, and nothing else. The direct analogue of Linux's schedstat
    /// `ttwu_count`, counted at `schedule_thread`.
    pub thread_wakes: u64,
    /// Threads pulled off another cpu's run queue by a rebalance.
    pub steals: u64,
    /// Requests the kernel sent to the pager, and the object pages those requests named. The
    /// pages figure includes read-ahead widening, so it exceeds what faults strictly needed.
    pub pager_requests: u64,
    pub pager_pages_requested: u64,
    /// Pages that arrived in pager completions, and of those the ones installed into an object.
    /// The gap is pages transferred and then dropped because the object already had them.
    pub pager_pages_delivered: u64,
    pub pager_pages_installed: u64,
    /// Page-data completions handled.
    pub pager_completions: u64,
    /// Requests outstanding to the pager right now. A level, not a total.
    pub pager_inflight: u64,
    /// Syscalls made since boot. The same figure [SyscallStats::nr_syscalls] reports; carried
    /// here so a reader wanting only the total need not fetch the per-syscall arrays, which are
    /// several KiB and get copied on every sample.
    pub syscalls: u64,
    /// Cumulative nanoseconds the hypervisor ran something else while our cpus were runnable
    /// (KVM steal time), summed over cpus. Zero on bare metal. Duplicated from
    /// [SysInfo::steal_ns] so a sampler polling rates does not have to read `SysInfo` too --
    /// everything else there is static.
    pub steal_ns: u64,
    /// Context switches by why the outgoing thread left its cpu, summed over cpus. They add up
    /// to `ctx_switches`. `switch_preempt` is every involuntary switch (a hardtick mark or a
    /// wake acted on, including at syscall exit), `switch_block` a thread that slept or
    /// suspended, `switch_from_idle` the idle thread handing a cpu to work.
    pub switch_exit: u64,
    pub switch_block: u64,
    pub switch_yield: u64,
    pub switch_preempt: u64,
    pub switch_from_idle: u64,
    /// Switches that left a cpu idle, a subset of `ctx_switches`.
    pub switch_to_idle: u64,
    /// Reschedules that ran the same thread again: a yield or preempt with nothing better queued.
    pub resched_noop: u64,
    /// Hardtick preempt marks by cause: the running thread's slice ran out with a peer queued,
    /// or a queued thread outranks it (a woken equal counts once it has had a tick).
    pub preempt_slice: u64,
    pub preempt_pri: u64,
    /// Where `select_cpu` sent threads, one count per pick: the thread's single allowed cpu;
    /// its last cpu, still warm and able to run it now; the nearest cache level with a cpu that
    /// runs it now (`near` is the last cpu's own node, `far` a wider level); its last cpu
    /// although cold, being no busier than the alternative; the least loaded cpu, to wait; or
    /// the affinity fallback before the topology existed. `pick_migrate` is additionally
    /// counted when the pick was not the thread's last cpu.
    pub pick_pinned: u64,
    pub pick_last_warm: u64,
    pub pick_near: u64,
    pub pick_far: u64,
    pub pick_last_cold: u64,
    pub pick_lowest: u64,
    pub pick_fallback: u64,
    pub pick_migrate: u64,
}

pub fn sys_kernel_stats() -> KernelStats {
    let mut stats = core::mem::MaybeUninit::<KernelStats>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut stats as *mut core::mem::MaybeUninit<KernelStats> as u64,
                InfoKind::KernelStats as u64,
            ],
        );
        stats.assume_init()
    }
}

#[derive(Debug, Copy, Clone, Default)]
#[repr(C)]
pub struct ObjectStats {
    pub nr_objects: usize,
    pub nr_mapped: usize,
    pub nr_pending_delete: usize,
    pub nr_handles: usize,
    pub nr_ties: usize,
}

pub fn sys_object_stats() -> ObjectStats {
    let mut stats = core::mem::MaybeUninit::<ObjectStats>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut stats as *mut core::mem::MaybeUninit<ObjectStats> as u64,
                InfoKind::ObjectStats as u64,
            ],
        );
        stats.assume_init()
    }
}

pub fn sys_sctx_stats() -> SctxStats {
    let mut sctx_stats = core::mem::MaybeUninit::<SctxStats>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut sctx_stats as *mut core::mem::MaybeUninit<SctxStats> as u64,
                InfoKind::SctxStats as u64,
            ],
        );
        sctx_stats.assume_init()
    }
}

pub fn sys_thread_stats() -> ThreadStats {
    let mut thread_stats = core::mem::MaybeUninit::<ThreadStats>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut thread_stats as *mut core::mem::MaybeUninit<ThreadStats> as u64,
                InfoKind::ThreadStats as u64,
            ],
        );
        thread_stats.assume_init()
    }
}

pub fn sys_memory_stats() -> MemoryStats {
    let mut memstats = core::mem::MaybeUninit::<MemoryStats>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut memstats as *mut core::mem::MaybeUninit<MemoryStats> as u64,
                InfoKind::MemoryStats as u64,
            ],
        );
        memstats.assume_init()
    }
}

pub fn sys_syscall_stats() -> SyscallStats {
    let mut stats = core::mem::MaybeUninit::<SyscallStats>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut stats as *mut core::mem::MaybeUninit<SyscallStats> as u64,
                InfoKind::SyscallStats as u64,
            ],
        );
        stats.assume_init()
    }
}

pub fn sys_lock_stats() -> LockStats {
    let mut stats = core::mem::MaybeUninit::<LockStats>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut stats as *mut core::mem::MaybeUninit<LockStats> as u64,
                InfoKind::LockStats as u64,
            ],
        );
        stats.assume_init()
    }
}

/// Get a SysInfo struct from the kernel.
pub fn sys_info() -> SysInfo {
    let mut sysinfo = core::mem::MaybeUninit::<SysInfo>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut sysinfo as *mut core::mem::MaybeUninit<SysInfo> as u64,
                InfoKind::SysInfo as u64,
            ],
        );
        sysinfo.assume_init()
    }
}

/// How [CpuInfo::cur_khz] was obtained.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum FreqSource {
    /// Nothing known; `cur_khz` and `nominal_khz` are 0.
    #[default]
    Unknown = 0,
    /// The cpu exposes no usable cycle counters, so `cur_khz` repeats `nominal_khz`.
    Nominal = 1,
    /// x86 APERF/MPERF.
    AperfMperf = 2,
    /// x86 fixed-function unhalted core and reference cycle counters.
    FixedCounters = 3,
    /// aarch64 Activity Monitors: core cycles against constant-rate cycles.
    Amu = 4,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum CacheKind {
    #[default]
    Unknown = 0,
    Data = 1,
    Instruction = 2,
    Unified = 3,
}

/// What a level of the cpu topology tree groups. Root-most first in [CpuInfo::levels].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum CpuTopoLevelKind {
    #[default]
    Other = 0,
    /// The whole machine.
    System = 1,
    Package = 2,
    Die = 3,
    Module = 4,
    /// A cluster of cores (aarch64 MPIDR affinity level above the core).
    Cluster = 5,
    /// A grouping that exists only because a cache is shared at it.
    Cache = 6,
    /// A core: the leaf, holding its SMT threads.
    Core = 7,
}

bitflags::bitflags! {
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
    pub struct CpuCacheFlags: u16 {
        const INCLUSIVE = 1;
        const FULLY_ASSOCIATIVE = 2;
    }
}

pub const CPU_INFO_MAX_CACHES: usize = 8;
pub const CPU_INFO_MAX_LEVELS: usize = 8;

/// One cache reachable from a cpu.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[repr(C)]
pub struct CpuCacheInfo {
    /// 1 is closest to the core.
    pub level: u8,
    pub kind: CacheKind,
    pub flags: CpuCacheFlags,
    /// Bytes per line.
    pub line_size: u32,
    /// Ways of associativity.
    pub ways: u32,
    pub sets: u32,
    /// Capacity in bytes.
    pub size: u64,
    /// Names this cache instance system-wide: cpus reporting the same `id` share the cache.
    pub id: u32,
    /// The [CpuTopoLevelInfo::id] of the node this cache is shared at.
    pub node: u32,
    /// Cpus sharing this cache.
    pub nr_sharing: u32,
    pub _pad: u32,
}

/// One node on a cpu's path through the topology tree.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[repr(C)]
pub struct CpuTopoLevelInfo {
    pub kind: CpuTopoLevelKind,
    /// Names the node system-wide: cpus reporting the same `id` at a level sit under one node.
    pub id: u32,
    /// Cpus under this node.
    pub nr_cpus: u32,
    pub _pad: u32,
}

/// Everything the kernel knows about one cpu. See [sys_cpu_info].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[repr(C)]
pub struct CpuInfo {
    pub version: u32,
    pub flags: u32,
    /// The dense index this was queried by, `0..cpu_count`.
    pub index: u32,
    /// The kernel's cpu id (x86: APIC id; aarch64: MPIDR affinity). Not dense; this is the id
    /// [super::ThreadSchedStats::cpu] reports and a [super::CpuMask] names.
    pub id: u32,
    /// Rated frequency, 0 when unknown.
    pub nominal_khz: u64,
    /// Frequency over the most recent sampling window, per [CpuInfo::freq_source].
    pub cur_khz: u64,
    pub freq_source: FreqSource,
    pub nr_levels: u32,
    /// Monotonic time of the sample behind `cur_khz`; 0 when it is not a sample.
    pub freq_sample_ns: u64,
    pub nr_caches: u32,
    pub _pad: u32,
    /// Root-most first; the last entry is this cpu's core.
    pub levels: [CpuTopoLevelInfo; CPU_INFO_MAX_LEVELS],
    /// Level 1 first.
    pub caches: [CpuCacheInfo; CPU_INFO_MAX_CACHES],
}

impl CpuInfo {
    pub fn levels(&self) -> &[CpuTopoLevelInfo] {
        &self.levels[..(self.nr_levels as usize).min(CPU_INFO_MAX_LEVELS)]
    }

    pub fn caches(&self) -> &[CpuCacheInfo] {
        &self.caches[..(self.nr_caches as usize).min(CPU_INFO_MAX_CACHES)]
    }

    pub fn cur_mhz(&self) -> u64 {
        self.cur_khz / 1000
    }
}

/// Describe the `index`th cpu, `0..sys_info().cpu_count()`. Fails with `INVALID_ARGUMENT` past
/// the end.
pub fn sys_cpu_info(index: usize) -> Result<CpuInfo, TwzError> {
    let mut info = core::mem::MaybeUninit::<CpuInfo>::zeroed();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[
                &mut info as *mut core::mem::MaybeUninit<CpuInfo> as u64,
                InfoKind::CpuInfo as u64,
                index as u64,
            ],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, _| unsafe { info.assume_init() },
        twzerr,
    )
}
