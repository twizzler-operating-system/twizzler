// use alloc::boxed::Box;

use crate::{
    arch::address::{PhysAddr, VirtAddr},
    // interrupt::Destination,
};

// TODO:
const MAX_INVALIDATION_INSTRUCTIONS: usize = 0;
#[derive(Clone)]
struct TlbInvData {
    // target_cr3: u64,
    instructions: [InvInstruction; MAX_INVALIDATION_INSTRUCTIONS],
    len: u8,
    flags: u8,
}

fn tlb_non_global_inv() {
    todo!("non global tlb invalidation")
}

fn tlb_global_inv() {
    todo!("tlb global invalidation")
}

impl TlbInvData {
    // TODO: tlb invalidation flags
    const GLOBAL: u8 = 0;
    const FULL: u8 = 0;

    fn set_global(&mut self) {
        self.flags |= Self::GLOBAL;
    }

    fn set_full(&mut self) {
        self.flags |= Self::FULL;
    }

    fn full(&self) -> bool {
        self.flags & Self::FULL != 0
    }

    fn global(&self) -> bool {
        self.flags & Self::GLOBAL != 0
    }

    fn target(&self) -> u64 {
        todo!()
    }

    fn instructions(&self) -> &[InvInstruction] {
        &self.instructions[0..(self.len as usize)]
    }

    fn enqueue(&mut self, inst: InvInstruction) {
        if inst.is_global() {
            self.set_global();
        }

        if self.len as usize == MAX_INVALIDATION_INSTRUCTIONS {
            self.set_full();
            return;
        }

        self.instructions[self.len as usize] = inst;
        self.len += 1;
    }

    unsafe fn do_invalidation(&self) {
        todo!()
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
struct InvInstruction(u64);

impl InvInstruction {
    fn new(_addr: VirtAddr, _is_global: bool, _is_terminal: bool, _level: u8) -> Self {
        todo!()
    }

    fn addr(&self) -> VirtAddr {
        todo!()
    }

    fn is_global(&self) -> bool {
        todo!()
    }

    fn is_terminal(&self) -> bool {
        todo!()
    }

    fn level(&self) -> u8 {
        todo!()
    }

    fn execute(&self) {
        todo!();
    }
}

#[derive(Default)]
/// An object that manages cache line invalidations during page table updates.
pub struct ArchCacheLineMgr {
    dirty: Option<u64>,
}

// TODO:
const CACHE_LINE_SIZE: u64 = 0;
impl ArchCacheLineMgr {
    /// Flush a given cache line when this [ArchCacheLineMgr] is dropped. Subsequent flush requests for the same cache
    /// line will be batched. Flushes for different cache lines will cause older requests to flush immediately, and the
    /// new request will be flushed when this object is dropped.
    pub fn flush(&mut self, _line: VirtAddr) {
        todo!()
    }

    fn do_flush(&mut self) {
        todo!()
    }
}

impl Drop for ArchCacheLineMgr {
    fn drop(&mut self) {
        self.do_flush();
    }
}

/// A management object for TLB invalidations that occur during a page table operation.
pub struct ArchTlbMgr {
    data: TlbInvData,
}

impl ArchTlbMgr {
    /// Construct a new [ArchTlbMgr].
    pub fn new(_target: PhysAddr) -> Self {
        todo!()
    }

    /// Enqueue a new TLB invalidation. is_global should be set iff the page is global, and is_terminal should be set
    /// iff the invalidation is for a leaf.
    pub fn enqueue(&mut self, _addr: VirtAddr, _is_global: bool, _is_terminal: bool, _level: usize) {
        todo!()
    }

    /// Execute all queued invalidations.
    pub fn finish(&mut self) {
        todo!()
    }
}
