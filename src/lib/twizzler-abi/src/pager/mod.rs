/// Submission data from the pager to the kernel.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum PagerRequest {
    Ping,
    GetMemoryPages(usize),
}

/// Completion data for pager to kernel queue.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum PagerCompletion {
    Ok,
    Err,
    MemoryPages(MemoryPages),
}

/// Submission data from the kernel to the.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum KernelRequest {
    Ping,
    ReserveSlot,
}

/// Completion data for kernel to pager queue.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum KernelCompletion {
    Ok,
    Err,
    Slot(usize),
}

bitflags::bitflags! {
    pub struct PhysicalAddrFlags : u32 {
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct PhysicalAddr(u64);

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct VirtualAddr(u64);

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MemoryPages {
    pub phys: PhysicalAddr,
    pub virt: VirtualAddr,
    pub num: usize,
}
