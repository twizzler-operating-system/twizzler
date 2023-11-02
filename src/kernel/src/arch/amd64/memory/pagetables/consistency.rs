use x86::controlregs::Cr4;

use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        interrupt::TLB_SHOOTDOWN_VECTOR,
        processor::{TlbShootdownInfo, NUM_TLB_SHOOTDOWN_ENTRIES},
    },
    interrupt::Destination,
    processor::{current_processor, spin_wait_until, tls_ready, with_each_active_processor},
};

const MAX_INVALIDATION_INSTRUCTIONS: usize = 16;
#[derive(Clone)]
pub struct TlbInvData {
    target_cr3: u64,
    instructions: [InvInstruction; MAX_INVALIDATION_INSTRUCTIONS],
    len: u8,
    flags: u8,
}

fn tlb_non_global_inv() {
    unsafe {
        let x = x86::controlregs::cr3();
        x86::controlregs::cr3_write(x);
    }
}

fn tlb_global_inv() {
    unsafe {
        let cr4 = x86::controlregs::cr4();
        if cr4.contains(Cr4::CR4_ENABLE_GLOBAL_PAGES) {
            let cr4_without_pge = cr4 & !Cr4::CR4_ENABLE_GLOBAL_PAGES;
            x86::controlregs::cr4_write(cr4_without_pge);
            x86::controlregs::cr4_write(cr4);
        } else {
            tlb_non_global_inv();
        }
    }
}

impl TlbInvData {
    const GLOBAL: u8 = 1;
    const FULL: u8 = 2;

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
        self.target_cr3
    }

    fn instructions(&self) -> &[InvInstruction] {
        &self.instructions[0..(self.len as usize)]
    }

    fn merge(&mut self, other: TlbInvData) {
        if other.target_cr3 != self.target_cr3 {
            self.set_global();
            self.set_full();
        } else {
            if other.full() {
                self.set_full();
            }
            if other.global() {
                self.set_global();
            }
            if self.len as usize + other.len as usize > MAX_INVALIDATION_INSTRUCTIONS {
                self.set_full();
            } else {
                for inst in other.instructions() {
                    self.enqueue(*inst)
                }
            }
        }
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

    pub fn has_invalidations(&self) -> bool {
        self.len > 0 || self.full()
    }

    fn reset(&mut self) {
        *self = Self::new(self.target());
        assert!(!self.has_invalidations());
    }

    fn do_invalidation(&self) {
        if !self.has_invalidations() {
            return;
        }
        let our_cr3 = unsafe { x86::controlregs::cr3() };
        /*
        logln!(
            "invalidation started on CPU {}: target = {:x} ({}) {} {}",
            crate::processor::current_processor().id,
            self.target(),
            if self.target() == our_cr3 || self.global() {
                "HIT"
            } else {
                "miss"
            },
            if self.global() { "GLOBAL" } else { "" },
            if self.full() { "FULL" } else { "" }
        );
        for inst in self.instructions() {
            logln!("   -> {:x} {}", inst.addr().raw(), inst.level());
        }
        */
        if our_cr3 != self.target() && !self.global() {
            return;
        }

        if self.full() {
            if self.global() {
                tlb_global_inv();
            } else {
                tlb_non_global_inv();
            }
            return;
        }

        for inst in self.instructions() {
            inst.execute();
        }
    }

    fn new(target: u64) -> Self {
        TlbInvData {
            target_cr3: target,
            instructions: [InvInstruction::new(
                unsafe { VirtAddr::new_unchecked(0) },
                false,
                false,
                0,
            ); MAX_INVALIDATION_INSTRUCTIONS],
            len: 0,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
struct InvInstruction(u64);

impl InvInstruction {
    fn new(addr: VirtAddr, is_global: bool, is_terminal: bool, level: u8) -> Self {
        let addr: u64 = addr.into();
        let val = addr
            | if is_global { 1 << 0 } else { 0 }
            | if is_terminal { 1 << 1 } else { 0 }
            | (level as u64) << 2;
        Self(val)
    }

    fn addr(&self) -> VirtAddr {
        let val = self.0 & 0xfffffffffffff000;
        val.try_into().unwrap()
    }

    fn is_global(&self) -> bool {
        self.0 & 1 != 0
    }

    fn is_terminal(&self) -> bool {
        self.0 & 2 != 0
    }

    fn level(&self) -> u8 {
        (self.0 >> 2 & 0xff) as u8
    }

    fn execute(&self) {
        let addr: u64 = self.addr().into();
        unsafe {
            core::arch::asm!("invlpg [{addr}]", addr = in(reg) addr);
        }
    }
}

#[derive(Default)]
/// An object that manages cache line invalidations during page table updates.
pub struct ArchCacheLineMgr {
    dirty: Option<u64>,
}

const CACHE_LINE_SIZE: u64 = 64;
impl ArchCacheLineMgr {
    /// Flush a given cache line when this [ArchCacheLineMgr] is dropped. Subsequent flush requests for the same cache
    /// line will be batched. Flushes for different cache lines will cause older requests to flush immediately, and the
    /// new request will be flushed when this object is dropped.
    pub fn flush(&mut self, line: VirtAddr) {
        let addr: u64 = line.into();
        let addr = addr & !(CACHE_LINE_SIZE - 1);
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
        if let Some(addr) = self.dirty.take() {
            unsafe {
                core::arch::asm!("clflush [{addr}]", addr = in(reg) addr);
            }
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
}

impl ArchTlbMgr {
    /// Construct a new [ArchTlbMgr].
    pub fn new(target: PhysAddr) -> Self {
        let this = Self {
            data: TlbInvData::new(target.into()),
        };
        assert!(!this.data.has_invalidations());
        this
    }

    /// Enqueue a new TLB invalidation. is_global should be set iff the page is global, and is_terminal should be set
    /// iff the invalidation is for a leaf.
    pub fn enqueue(&mut self, addr: VirtAddr, is_global: bool, is_terminal: bool, level: usize) {
        self.data.enqueue(InvInstruction::new(
            addr,
            is_global,
            is_terminal,
            level as u8,
        ));
    }

    /// Execute all queued invalidations.
    pub fn finish(&mut self) {
        if !self.data.has_invalidations() {
            return;
        }
        let proc = current_processor();

        let mut count = 0;
        // Distribute the invalidation commands
        with_each_active_processor(|p| {
            if p.id != proc.id {
                p.arch.tlb_shootdown_info.lock().insert(self.data.clone());
                count += 1;
            }
        });
        if count > 0 {
            // Send the IPI, and then do local invalidations.
            super::super::super::apic::send_ipi(Destination::AllButSelf, TLB_SHOOTDOWN_VECTOR);
        }
        self.data.do_invalidation();

        if count > 0 {
            // Wait for each processor to report that it is done.
            with_each_active_processor(|p| {
                if p.id != proc.id {
                    spin_wait_until(|| !p.arch.tlb_shootdown_info.lock().is_finished(), || {});
                }
            });
        }
        self.data.reset();
    }
}

impl Drop for ArchTlbMgr {
    fn drop(&mut self) {
        // Only matters once other CPUs are setup, which only happens after TLS is ready
        if tls_ready() {
            self.finish();
        }
    }
}

pub fn tlb_shootdown_handler() {
    let cur = current_processor();
    let mut tlb_shootdown_info = cur.arch.tlb_shootdown_info.lock();
    tlb_shootdown_info.complete();
}

impl TlbShootdownInfo {
    pub fn insert(&mut self, data: TlbInvData) {
        // Try to find an empty slot
        for entry in &mut self.data {
            if entry.is_none() {
                *entry = Some(data);
                return;
            }
        }
        // Try to find a slot with the same target_cr3
        for entry in &mut self.data {
            // Unwrap-Ok: we know that all slots are Some from the first loop.
            if entry.as_ref().unwrap().target() == data.target() {
                entry.as_mut().unwrap().merge(data);
                return;
            }
        }
        // Choose the 0'th entry because if this makes it a full or global entry, we want to be able to
        // exit the handling loop early.
        // Unwrap-Ok: we know that all slots are Some from the first loop.
        self.data[0].as_mut().unwrap().merge(data);
    }

    pub fn is_finished(&self) -> bool {
        self.data.iter().all(Option::is_none)
    }

    pub fn complete(&mut self) {
        for entry in &mut self.data {
            if let Some(data) = entry.take() {
                data.do_invalidation();
                if data.full() && data.global() {
                    // Any other invalidations don't matter.
                    self.reset();
                    return;
                }
            }
        }
        // explicit reset not needed because we've called take() on all entries
    }

    fn reset(&mut self) {
        for i in 0..NUM_TLB_SHOOTDOWN_ENTRIES {
            self.data[i] = None;
        }
    }
}
