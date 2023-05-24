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
    // instructions: [InvInstruction; MAX_INVALIDATION_INSTRUCTIONS],
    len: u8,
    // flags: u8,
    addresses: [
        VirtAddr; Self::MAX_OUTSTANDING_INVALIDATIONS
    ],
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

    // the maximum number of tlb invalidations that can
    // be enqueued
    const MAX_OUTSTANDING_INVALIDATIONS: usize = 16;

    // fn set_global(&mut self) {
    //     self.flags |= Self::GLOBAL;
    // }

    // fn set_full(&mut self) {
    //     self.flags |= Self::FULL;
    // }

    // fn full(&self) -> bool {
    //     self.flags & Self::FULL != 0
    // }

    // fn global(&self) -> bool {
    //     self.flags & Self::GLOBAL != 0
    // }

    // fn target(&self) -> u64 {
    //     todo!()
    // }

    // fn instructions(&self) -> &[InvInstruction] {
    //     &self.instructions[0..(self.len as usize)]
    // }

    // fn enqueue(&mut self, inst: InvInstruction) {
    //     if inst.is_global() {
    //         self.set_global();
    //     }

    //     if self.len as usize == MAX_INVALIDATION_INSTRUCTIONS {
    //         self.set_full();
    //         return;
    //     }

    //     self.instructions[self.len as usize] = inst;
    //     self.len += 1;
    // }

    // unsafe fn do_invalidation(&self) {
    //     todo!()
    // }
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
    dirty: Option<u64>, // a single cacheline address to flush
}

// TODO:
const CACHE_LINE_SIZE: u64 = 64;
impl ArchCacheLineMgr {
    /// Flush a given cache line when this [ArchCacheLineMgr] is dropped. Subsequent flush requests for the same cache
    /// line will be batched. Flushes for different cache lines will cause older requests to flush immediately, and the
    /// new request will be flushed when this object is dropped.
    pub fn flush(&mut self, line: VirtAddr) {
        // logln!("[arch::cacheln] flush called on: {:#018x}", line.raw());
        let addr: u64 = line.into();
        // TODO: not sure if we need this step
        // let addr = addr & !(CACHE_LINE_SIZE - 1);
        if let Some(dirty) = self.dirty {
            if dirty != addr {
                self.do_flush();
                self.dirty = Some(addr);
            }
        } else {
            self.dirty = Some(addr);
        }
    }

    fn do_flush(&mut self) {
        if let Some(_addr) = self.dirty {
            // logln!("[arch::cacheln] flush something: {:#018x}", addr);
            unsafe {
                core::arch::asm!(
                    // flush the data part of the cache line
                    // TODO: do we need to use `dc cvac addr`?
                    // ensure the change to the table entry is visible to the MMU
                    "dsb ishst",
                    // ensure that the dsb has completed before the next instruction
                    "isb",
                );
            }
        } else {
            // logln!("[arch::cacheln] flush something: None");
        }
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
    root: PhysAddr
}

impl ArchTlbMgr {
    /// Construct a new [ArchTlbMgr].
    pub fn new(table_root: PhysAddr) -> Self {
        Self {
            data: TlbInvData { 
                addresses: [
                    unsafe {
                        VirtAddr::new_unchecked(0)
                    }; TlbInvData::MAX_OUTSTANDING_INVALIDATIONS
                ],
                len: 0,
            },
            root: table_root,
        }
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
