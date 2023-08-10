// use alloc::boxed::Box;

use crate::{
    arch::address::{PhysAddr, VirtAddr},
    // interrupt::Destination,
};

#[derive(Default)]
/// An object that manages cache line invalidations during page table updates.
pub struct ArchCacheLineMgr {
    dirty: Option<u64>, // a single cacheline address to flush
}

impl ArchCacheLineMgr {
    /// Flush a given cache line when this [ArchCacheLineMgr] is dropped. Subsequent flush requests for the same cache
    /// line will be batched. Flushes for different cache lines will cause older requests to flush immediately, and the
    /// new request will be flushed when this object is dropped.
    pub fn flush(&mut self, line: VirtAddr) {
        // logln!("[arch::cacheln] flush called on: {:#018x}", line.raw());
        let addr: u64 = line.into();
        // According to the AArch64 instruction manual:
        // "No alignment restrictions apply to this VA."
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
        if let Some(addr) = self.dirty {
            unsafe {
                core::arch::asm!(
                    // clean to point of coherency so all observers see the same thing
                    // dc - data cache
                    // cvac - clean by va to point of coherency
                    "dc cvac, {}",
                    // ensure the change to the table entry is visible to the MMU
                    "dsb ishst",
                    // ensure that the dsb has completed before the next instruction
                    "isb",
                    in(reg) addr
                );
            }
        }
    }
}

impl Drop for ArchCacheLineMgr {
    fn drop(&mut self) {
        self.do_flush();
    }
}

#[derive(Clone, Copy, Default)]
struct TlbInvData(u64);

impl TlbInvData {
    const TLBI_SHIFT: usize = 12;
    fn new(addr: VirtAddr) -> Self {
        let va: u64 = addr.into();
        TlbInvData(va >> Self::TLBI_SHIFT)
    }

    fn data(&self) -> u64 {
        self.0
    }

    fn addr(&self) -> u64 {
        self.0 << Self::TLBI_SHIFT
    }

    fn execute(&self) {
        // logln!("[arch::tlb] addr: {:#018x}", self.addr());
        // TODO: can we batch sync barriers?
        unsafe {
            core::arch::asm!(
                // wait for other data modifications to finish
                "dsb ishst",
                // e1 - EL1
                // va - by virtual address
                // is - inner sharable
                "tlbi vae1is, {}",
                // wait for tlbi instruction to finish
                "dsb ish",
                // wait for data sync barrier to finish
                "isb",
                in(reg) self.data()
            );
        }
    }
}

// A queue of TLB invalidations containg the data arguments
struct TlbInvQueue {
    data: [TlbInvData; Self::MAX_OUTSTANDING_INVALIDATIONS],
    len: u8
}

impl TlbInvQueue {
    const MAX_OUTSTANDING_INVALIDATIONS: usize = 16;

    fn new() -> Self {
        Self { 
            data: [TlbInvData::default(); Self::MAX_OUTSTANDING_INVALIDATIONS], 
            len: 0 
        }
    }

    fn enqueue(&mut self, addr: VirtAddr) {
        // check if the queue is full
        if self.is_full() {
            self.drain();
        }
        // enqueue tlb invalidation data
        let next = self.len as usize;
        self.data[next] = TlbInvData::new(addr);
        self.len += 1;
    }

    fn is_full(&self) -> bool {
        self.len as usize == Self::MAX_OUTSTANDING_INVALIDATIONS
    }

    fn drain(&mut self) {
        for i in 0..self.len as usize {
            let inv = &self.data[i];
            inv.execute();
        }
        self.len = 0;
    }
}

/// A management object for TLB invalidations that occur during a page table operation.
pub struct ArchTlbMgr {
    queue: TlbInvQueue,
    root: PhysAddr
}

impl ArchTlbMgr {
    /// Construct a new [ArchTlbMgr].
    pub fn new(table_root: PhysAddr) -> Self {
        Self {
            queue: TlbInvQueue::new(),
            root: table_root,
        }
    }

    /// Enqueue a new TLB invalidation. is_global should be set iff the page is global, and is_terminal should be set
    /// iff the invalidation is for a leaf.
    pub fn enqueue(&mut self, addr: VirtAddr, _is_global: bool, is_terminal: bool, _level: usize) {
        // only invalidate leaves
        if is_terminal {
            self.queue.enqueue(addr);
        }
    }

    /// Execute all queued invalidations.
    pub fn finish(&mut self) {
        self.queue.drain()
    }
}

impl Drop for ArchTlbMgr {
    fn drop(&mut self) {
        self.finish();
    }
}