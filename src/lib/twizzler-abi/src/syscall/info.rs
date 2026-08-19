use core::num::NonZeroUsize;

use twizzler_rt_abi::error::TwzError;

use super::Syscall;
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
