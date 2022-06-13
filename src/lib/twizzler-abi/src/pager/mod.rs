/// Submission data from the pager to the kernel.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum PagerRequest {
    Ping,
}

/// Completion data for pager to kernel queue.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum PagerCompletion {
    Ok,
    Err,
}

/// Submission data from the kernel to the.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum KernelRequest {
    Ping,
}

/// Completion data for kernel to pager queue.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum KernelCompletion {
    Ok,
    Err,
}

bitflags::bitflags! {
    pub struct PhysicalAddrFlags : u32 {
    }
}

/// Completion data for kernel to pager queue.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PhysicalAddr {
    pub addr: u64,
    pub flags: PhysicalAddrFlags,
}
