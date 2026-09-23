use core::sync::atomic::Ordering;

use twizzler_abi::syscall::{
    CPU_INFO_MAX_CACHES, CPU_INFO_MAX_LEVELS, CpuCacheFlags, CpuCacheInfo, CpuInfo,
    CpuTopoLevelInfo, CpuTopoLevelKind, InfoKind, KernelStats, LockStats, MemoryStats, SctxStats,
    SysInfo, SyscallStats, ThreadStats,
};
use twizzler_rt_abi::error::TwzError;

use crate::processor::{
    mp::{all_processors, with_each_active_processor},
    sched::{CPUTopoNode, CPUTopoType, get_cpu_topology},
};

type Result<T> = core::result::Result<T, TwzError>;

fn level_kind(kind: CPUTopoType) -> CpuTopoLevelKind {
    match kind {
        CPUTopoType::System => CpuTopoLevelKind::System,
        CPUTopoType::Package => CpuTopoLevelKind::Package,
        CPUTopoType::Die => CpuTopoLevelKind::Die,
        CPUTopoType::Module => CpuTopoLevelKind::Module,
        CPUTopoType::Cluster => CpuTopoLevelKind::Cluster,
        CPUTopoType::Cache => CpuTopoLevelKind::Cache,
        CPUTopoType::Core => CpuTopoLevelKind::Core,
        CPUTopoType::Other => CpuTopoLevelKind::Other,
    }
}

/// Fill `info` for the `index`th running cpu.
fn write_cpu_info(info: &mut CpuInfo, index: usize) -> Result<()> {
    let mut nth = 0;
    let mut found = None;
    with_each_active_processor(|p| {
        if nth == index {
            found = Some(p);
        }
        nth += 1;
    });
    let Some(p) = found else {
        return Err(TwzError::INVALID_ARGUMENT);
    };
    *info = CpuInfo::default();
    info.version = 1;
    info.index = index as u32;
    info.id = p.id;
    info.nominal_khz = crate::arch::freq::nominal_khz();
    let (khz, source, sample_ns) = p.freq.current();
    info.cur_khz = khz;
    info.freq_source = source;
    info.freq_sample_ns = sample_ns;

    // Leaf to root, then reported root-first; caches come out leaf-first, i.e. L1 first.
    let mut chain: heapless::Vec<&CPUTopoNode, 16> = heapless::Vec::new();
    let mut node = get_cpu_topology().find_cpu(p.id);
    while let Some(n) = node {
        if chain.push(n).is_err() {
            break;
        }
        node = n.parent();
    }
    for node in chain.iter().rev() {
        if info.nr_levels as usize >= CPU_INFO_MAX_LEVELS {
            break;
        }
        info.levels[info.nr_levels as usize] = CpuTopoLevelInfo {
            kind: level_kind(node.kind()),
            id: node.id(),
            nr_cpus: node.count() as u32,
            _pad: 0,
        };
        info.nr_levels += 1;
    }
    for node in chain.iter() {
        for (id, cache) in node.caches() {
            if info.nr_caches as usize >= CPU_INFO_MAX_CACHES {
                break;
            }
            let mut flags = CpuCacheFlags::empty();
            flags.set(CpuCacheFlags::INCLUSIVE, cache.inclusive);
            flags.set(CpuCacheFlags::FULLY_ASSOCIATIVE, cache.fully_assoc);
            info.caches[info.nr_caches as usize] = CpuCacheInfo {
                level: cache.level,
                kind: cache.kind,
                flags,
                line_size: cache.line_size,
                ways: cache.ways,
                sets: cache.sets,
                size: cache.size,
                id: *id,
                node: node.id(),
                nr_sharing: node.count() as u32,
                _pad: 0,
            };
            info.nr_caches += 1;
        }
    }
    Ok(())
}

pub fn write_sys_info_values(ptr: *mut u8, kind: InfoKind, arg: u64) -> Result<()> {
    match kind {
        InfoKind::CpuInfo => {
            let info: &mut CpuInfo = unsafe { &mut *(ptr as *mut CpuInfo) };
            write_cpu_info(info, arg as usize)
        }
        InfoKind::SysInfo => {
            let info: &mut SysInfo = unsafe { &mut *(ptr as *mut SysInfo) };
            info.cpu_count = all_processors().iter().fold(0, |acc, p| {
                acc + match &p {
                    Some(p) => {
                        if p.is_running() {
                            1
                        } else {
                            0
                        }
                    }
                    None => 0,
                }
            });
            info.flags = 0;
            info.version = 2;
            info.page_size = 0x1000;
            // Steal is per-cpu but reported whole-system: the reader's question is "was this
            // host contended", not "which vcpu paid" -- schedmon carries the per-cpu split.
            #[cfg(target_arch = "x86_64")]
            {
                let mut steal = 0u64;
                crate::processor::mp::with_each_active_processor(|p| {
                    steal += crate::arch::kvm::steal_time_ns(p);
                });
                info.steal_ns = steal;
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                info.steal_ns = 0;
            }
            Ok(())
        }
        InfoKind::MemoryStats => {
            let stats: &mut MemoryStats = unsafe { &mut *(ptr as *mut MemoryStats) };
            *stats = crate::memory::get_memory_stats();
            Ok(())
        }
        InfoKind::ThreadStats => {
            let stats: &mut ThreadStats = unsafe { &mut *(ptr as *mut ThreadStats) };
            *stats = crate::thread::get_thread_stats();
            Ok(())
        }
        InfoKind::SctxStats => {
            let stats: &mut SctxStats = unsafe { &mut *(ptr as *mut SctxStats) };
            *stats = crate::security::get_sctx_stats();
            Ok(())
        }
        InfoKind::LockStats => {
            let stats: &mut LockStats = unsafe { &mut *(ptr as *mut LockStats) };
            *stats = crate::mutex::get_lock_stats();
            Ok(())
        }
        InfoKind::SyscallStats => {
            let stats: &mut SyscallStats = unsafe { &mut *(ptr as *mut SyscallStats) };
            *stats = crate::syscall::get_syscall_stats();
            Ok(())
        }
        InfoKind::ObjectStats => {
            let stats: &mut twizzler_abi::syscall::ObjectStats =
                unsafe { &mut *(ptr as *mut twizzler_abi::syscall::ObjectStats) };
            *stats = crate::obj::get_object_stats();
            Ok(())
        }
        InfoKind::KernelStats => {
            let stats: &mut KernelStats = unsafe { &mut *(ptr as *mut KernelStats) };
            *stats = KernelStats::default();
            stats.version = 1;
            // Scheduling counters are per-cpu; the question they answer here is system-wide, so
            // they are summed exactly as the syscall and fault counts are.
            crate::processor::mp::with_each_active_processor(|p| {
                stats.hardticks += p.stats.hardticks.load(Ordering::Relaxed);
                stats.ctx_switches += p.stats.switches.load(Ordering::Relaxed);
                stats.preempts += p.stats.preempts.load(Ordering::Relaxed);
                stats.wakeups += p.stats.wakeups.load(Ordering::Relaxed);
                stats.steals += p.stats.steals.load(Ordering::Relaxed);
                stats.switch_exit += p.stats.switch_exit.load(Ordering::Relaxed);
                stats.switch_block += p.stats.switch_block.load(Ordering::Relaxed);
                stats.switch_yield += p.stats.switch_yield.load(Ordering::Relaxed);
                stats.switch_preempt += p.stats.switch_preempt.load(Ordering::Relaxed);
                stats.switch_from_idle += p.stats.switch_from_idle.load(Ordering::Relaxed);
                stats.switch_to_idle += p.stats.switch_to_idle.load(Ordering::Relaxed);
                stats.resched_noop += p.stats.resched_noop.load(Ordering::Relaxed);
                stats.preempt_slice += p.stats.preempt_slice.load(Ordering::Relaxed);
                stats.preempt_pri += p.stats.preempt_pri.load(Ordering::Relaxed);
                stats.pick_pinned += p.stats.pick_pinned.load(Ordering::Relaxed);
                stats.pick_last_warm += p.stats.pick_last_warm.load(Ordering::Relaxed);
                stats.pick_near += p.stats.pick_near.load(Ordering::Relaxed);
                stats.pick_far += p.stats.pick_far.load(Ordering::Relaxed);
                stats.pick_last_cold += p.stats.pick_last_cold.load(Ordering::Relaxed);
                stats.pick_lowest += p.stats.pick_lowest.load(Ordering::Relaxed);
                stats.pick_fallback += p.stats.pick_fallback.load(Ordering::Relaxed);
                stats.pick_migrate += p.stats.pick_migrate.load(Ordering::Relaxed);
            });
            // Global, not per-cpu: charged at the wake site rather than to a cpu, so it is read
            // once outside the fold above.
            stats.thread_wakes = crate::processor::sched::wakesrc::total();
            stats.interrupts = crate::interrupt::taken();
            let pager = crate::pager::pager_totals();
            stats.pager_requests = pager.requests;
            stats.pager_pages_requested = pager.pages_requested;
            stats.pager_pages_delivered = pager.pages_delivered;
            stats.pager_pages_installed = pager.pages_installed;
            stats.pager_completions = pager.completions;
            stats.pager_inflight = crate::pager::live_requests() as u64;
            stats.syscalls = crate::syscall::nr_syscalls() as u64;
            // Same source as the SysInfo arm's `steal_ns`; see the note there on why a per-cpu
            // quantity is reported whole-system.
            #[cfg(target_arch = "x86_64")]
            {
                let mut steal = 0u64;
                crate::processor::mp::with_each_active_processor(|p| {
                    steal += crate::arch::kvm::steal_time_ns(p);
                });
                stats.steal_ns = steal;
            }
            Ok(())
        }
    }
}
