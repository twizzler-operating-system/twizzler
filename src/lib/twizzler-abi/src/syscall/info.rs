use core::num::NonZeroUsize;

use super::Syscall;
use crate::arch::syscall::raw_syscall;
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

/// Get a SysInfo struct from the kernel.
pub fn sys_info() -> SysInfo {
    let mut sysinfo = core::mem::MaybeUninit::<SysInfo>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[&mut sysinfo as *mut core::mem::MaybeUninit<SysInfo> as u64],
        );
        sysinfo.assume_init()
    }
}
