use twizzler_abi::syscall::{
    InfoKind, KallocCensus, LockStats, MemoryStats, SctxStats, SysInfo, SyscallStats, ThreadStats,
};

use crate::processor::mp::all_processors;

type Result<T> = core::result::Result<T, twizzler_rt_abi::error::TwzError>;

pub fn write_sys_info_values(ptr: *mut u8, kind: InfoKind) -> Result<()> {
    match kind {
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
        InfoKind::KallocCensus => {
            let census: &mut KallocCensus = unsafe { &mut *(ptr as *mut KallocCensus) };
            crate::memory::kalloc_census::fill(census);
            Ok(())
        }
        InfoKind::KallocTrack => {
            let ctl: &mut twizzler_abi::syscall::KallocTrackCtl =
                unsafe { &mut *(ptr as *mut twizzler_abi::syscall::KallocTrackCtl) };
            crate::memory::kalloc_track::control(ctl);
            Ok(())
        }
    }
}
