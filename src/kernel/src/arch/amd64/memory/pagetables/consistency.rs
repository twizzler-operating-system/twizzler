use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use x86::controlregs::Cr4;

use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        interrupt::TLB_SHOOTDOWN_VECTOR,
    },
    interrupt::{self, Destination},
    memory::pagetables::{
        MappingCursor, tlb_shootdown_inc_count, trace_tlb_invalidation, trace_tlb_shootdown,
    },
    arch::processor::CR3_IN_TRANSITION,
    processor::{
        Processor,
        mp::{current_processor, with_each_active_processor},
        sched::CpuSet,
        spin_wait_until, tls_ready,
    },
    thread::current_thread_ref,
};

const MAX_INVALIDATION_INSTRUCTIONS: usize = 16;
#[derive(Clone, Debug, Copy)]
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
        }
        tlb_non_global_inv();
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

    /// Whether `p` needs to be sent (and waited on for) this invalidation. A processor
    /// whose active address space doesn't match our target can't hold stale entries for
    /// it: it's either off on an unrelated context now, or it'll switch into ours later,
    /// which does a full non-global flush via its own `mov cr3` (no PCID is in use here)
    /// -- by then the underlying page-table write that triggered this invalidation has
    /// already happened, so it'll walk fresh, correct PTEs. Global invalidations always
    /// go to every processor regardless, matching the receiver-side check in
    /// `do_invalidation`. A processor midway through a page-table switch publishes
    /// `CR3_IN_TRANSITION`, which matches here, because it may hold entries for either root.
    fn should_target(&self, p: &Processor) -> bool {
        if self.global() {
            return true;
        }
        let active = p.arch.active_cr3.load(Ordering::Acquire);
        active == CR3_IN_TRANSITION || active == self.target()
    }

    fn apply_offset(&self, map: &MappingCursor) -> Self {
        let mut new_data = *self;

        for inst in new_data.instructions.iter_mut().take(self.len as usize) {
            let addr: u64 = inst.addr().into();
            let new_addr = addr + map.start().raw() as u64;
            // TODO: if the address is not covered in map, then skip this one.
            *inst = InvInstruction::new(
                unsafe { VirtAddr::new_unchecked(new_addr) },
                inst.is_global(),
                inst.is_terminal(),
                inst.level(),
            );
        }
        new_data
    }

    fn merge_ignoring_target(&mut self, other: &Self) {
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

    fn merge(&mut self, other: TlbInvData) {
        // If these two target different page tables, then there's nothing we can do but flush all.
        if other.target_cr3 != self.target_cr3 {
            self.set_global();
            self.set_full();
        } else {
            // Otherwise, the flags are OR'd, and the instructions concatenated. Order doesn't
            // matter. If we'd have too many instructions, just fall back to full
            // invalidation.
            self.merge_ignoring_target(&other);
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
        // If none of the commands are global, and it's targeting a different set of
        // page tables than is active, then we can ignore it.
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

#[derive(Clone, Copy)]
#[repr(transparent)]
// Stores an address along with a few fields, like level, is_global. Since addresses
// here are page aligned, we have room in the bottom bits so we can pack this into a u64.
struct InvInstruction(u64);

impl core::fmt::Debug for InvInstruction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InvInstruction")
            .field("addr", &self.addr())
            .finish_non_exhaustive()
    }
}

impl InvInstruction {
    const ADDR_MASK: u64 = !0xfff;
    fn new(addr: VirtAddr, is_global: bool, is_terminal: bool, level: u8) -> Self {
        let addr: u64 = addr.into();
        let val = (addr & Self::ADDR_MASK)
            | if is_global { 1 << 0 } else { 0 }
            | if is_terminal { 1 << 1 } else { 0 }
            | (level as u64) << 2;
        Self(val)
    }

    fn addr(&self) -> VirtAddr {
        let val = self.0 & Self::ADDR_MASK;
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
    /// Flush a given cache line when this [ArchCacheLineMgr] is dropped. Subsequent flush requests
    /// for the same cache line will be batched. Flushes for different cache lines will cause
    /// older requests to flush immediately, and the new request will be flushed when this
    /// object is dropped.
    pub fn add_cache_line(&mut self, line: VirtAddr) {
        let addr: u64 = line.into();
        let addr = addr & !(CACHE_LINE_SIZE - 1);
        if let Some(dirty) = self.dirty {
            if dirty != addr {
                self.flush();
                self.dirty = Some(addr);
            }
        } else {
            self.dirty = Some(addr);
        }
    }

    pub fn flush(&mut self) {
        if let Some(addr) = self.dirty.take() {
            unsafe {
                core::arch::asm!("clflush [{addr}]", addr = in(reg) addr);
            }
        }
    }
}

impl Drop for ArchCacheLineMgr {
    fn drop(&mut self) {
        self.flush();
    }
}

/// A management object for TLB invalidations that occur during a page table operation.
#[derive(Clone)]
pub struct ArchTlbMgr {
    data: TlbInvData,
}

impl core::fmt::Debug for ArchTlbMgr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ArchTlbMgr")
            .field("target", &self.data.target())
            .field("full", &self.data.full())
            .field("global", &self.data.global())
            .field("instructions", &self.data.instructions())
            .finish()
    }
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

    pub fn new_full_global() -> Self {
        let mut this = Self::new(PhysAddr::new(0).unwrap());
        this.set_full_global();
        this
    }

    pub fn set_full_global(&mut self) {
        self.data.set_full();
        self.data.set_global();
    }

    pub fn is_full(&self) -> bool {
        self.data.full()
    }

    pub fn set_target(&mut self, target: PhysAddr) {
        self.data.target_cr3 = target.into();
    }

    pub fn reset(&mut self) {
        self.data.reset();
    }

    pub fn apply_offset_from_map(&self, map: &MappingCursor) -> Self {
        let data = self.data.apply_offset(map);
        Self { data }
    }

    pub fn merge(&mut self, other: Self) {
        self.data.merge(other.data);
    }

    /// Enqueue a new TLB invalidation. is_global should be set iff the page is global, and
    /// is_terminal should be set iff the invalidation is for a leaf.
    pub fn enqueue(&mut self, addr: VirtAddr, is_global: bool, is_terminal: bool, level: usize) {
        self.data.enqueue(InvInstruction::new(
            addr,
            is_global,
            is_terminal,
            level as u8,
        ));
    }

    pub fn has_pending(&self) -> bool {
        self.data.has_invalidations()
    }

    /// Execute all queued invalidations.
    pub fn finish(&mut self) {
        if !tls_ready() {
            self.reset();
            return;
        }
        if !self.data.has_invalidations() {
            return;
        }

        let ct = current_thread_ref();
        let _guard = ct.as_ref().map(|ct| ct.enter_critical());
        // We definitely don't want to reschedule to a different CPU while doing this.
        let proc = current_processor();

        let mut count = 0;
        // Our caller's page-table writes must be visible to any processor that we then decide
        // *not* to target. Those writes and the `active_cr3` loads below form a store->load
        // pair, the one reordering x86 permits, so without this fence we could observe a
        // processor's pre-switch cr3 while it observes our pre-unmap PTEs -- and we would skip
        // it. Pairs with the SeqCst store in `ArchContext::switch_to_target`.
        core::sync::atomic::fence(Ordering::SeqCst);
        // Distribute the invalidation commands, recording exactly who we sent to. `should_target`
        // reads each processor's active cr3, which can change underneath us, so the wait below has
        // to use the set we actually sent to rather than re-evaluating the predicate against a
        // cr3 that has since moved on.
        let mut targets = CpuSet::empty();
        with_each_active_processor(|p| {
            if p.id != proc.id && self.data.should_target(p) {
                p.arch.tlb_shootdown_info.insert(self.data.clone());
                targets.insert(p.id);
                count += 1;
            }
        });
        tlb_shootdown_inc_count(count > 0);
        if count > 0 {
            trace_tlb_shootdown();
            // Send the IPI, and then do local invalidations.
            super::super::super::apic::send_ipi(Destination::AllButSelf, TLB_SHOOTDOWN_VECTOR);
        }
        trace_tlb_invalidation();
        self.data.do_invalidation();

        if count > 0 {
            // Wait for every targeted processor to report that it is done. This wait must not be
            // bounded: our caller frees the unmapped frames -- page table pages included -- as soon
            // as we return, so giving up early lets a processor that still holds stale entries walk
            // recycled memory. It cannot deadlock, because spin_wait_until services our own
            // incoming shootdowns on every pass.
            //
            // Resend periodically rather than on every pass: a processor that had interrupts
            // disabled when the broadcast went out needs another nudge, but hammering its APIC only
            // delays the acknowledgement we're waiting for.
            // TODO: targeted shootdown and pcid tracking would cut how often we get here at all.
            const RESEND_INTERVAL: usize = 4096;
            const WARN_INTERVAL: usize = 1 << 22;
            with_each_active_processor(|p| {
                if !targets.contains(p.id) {
                    return;
                }
                let mut iters: usize = 0;
                spin_wait_until(
                    || {
                        if p.arch.tlb_shootdown_info.is_finished() {
                            return Some(());
                        }
                        iters += 1;
                        if iters % RESEND_INTERVAL == 0 {
                            super::super::super::apic::send_ipi(
                                Destination::Single(p.id),
                                TLB_SHOOTDOWN_VECTOR,
                            );
                        }
                        if iters % WARN_INTERVAL == 0 {
                            logln!(
                                "warning -- TLB shootdown stalled on CPUs {} -> {} ({} iterations)",
                                proc.id,
                                p.id,
                                iters
                            );
                        }
                        None
                    },
                    || {},
                );
            });
        }
        drop(_guard);
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
    // Interrupts are probably disabled here, but ensure it anyway.
    interrupt::with_disabled(|| {
        let cur = current_processor();
        cur.arch.tlb_shootdown_info.complete();
    })
}

const NUM_TLB_SHOOTDOWN_ENTRIES: usize = 4;
pub struct TlbShootdownInfo {
    // We use a manual spin lock, here, because the general spinlock code actually calls
    // into this code to poll for TLB shootdowns to avoid deadlock. Hence, we have to manually
    // lock here. This is "safe" because we fully control any code run while holding the lock,
    // and we can guarantee that we don't wait on any other locks.
    lock: AtomicBool,
    // Maintain a list of a few invalidation command slots we can use, in case multiple CPUs send
    // out invalidation commands at the same time. Note that in the case that this array is full of
    // entries, we just merge any incoming commands into another command. This is possible because
    // there is always a least-upper-bound merge between two invalidation commands that always
    // invalidates all data from both commands. In the worst case, this merge is simply a full,
    // global invalidation.
    data: UnsafeCell<[Option<TlbInvData>; NUM_TLB_SHOOTDOWN_ENTRIES]>,
    full_invl: AtomicBool,
}

impl TlbShootdownInfo {
    pub fn new() -> Self {
        Self {
            data: UnsafeCell::new([None, None, None, None]),
            lock: AtomicBool::new(false),
            full_invl: AtomicBool::new(false),
        }
    }

    pub fn insert(&self, new_data: TlbInvData) {
        interrupt::with_disabled(|| {
            let mut iters = 0;
            while self.lock.swap(true, Ordering::Acquire) {
                iters += 1;
                if iters >= 100 {
                    log::warn!("failed to insert tlb shootdown info -- setting full_invl");
                    self.full_invl.store(true, Ordering::SeqCst);
                    return;
                }
                core::hint::spin_loop()
            }
            let data = unsafe { self.data.get().as_mut().unwrap() };
            // Try to find an empty slot
            for entry in data.iter_mut() {
                if entry.is_none() {
                    *entry = Some(new_data);
                    self.lock.store(false, Ordering::Release);
                    return;
                }
            }
            // Try to find a slot with the same target_cr3
            for entry in data.iter_mut() {
                // Unwrap-Ok: we know that all slots are Some from the first loop.
                if entry.as_ref().unwrap().target() == new_data.target() {
                    entry.as_mut().unwrap().merge(new_data);
                    self.lock.store(false, Ordering::Release);
                    return;
                }
            }
            // Choose the 0'th entry because if this makes it a full or global entry, we want to be
            // able to exit the handling loop early.
            // Unwrap-Ok: we know that all slots are Some from the first loop.
            data[0].as_mut().unwrap().merge(new_data);
            self.lock.store(false, Ordering::Release);
        })
    }

    pub fn is_finished(&self) -> bool {
        interrupt::with_disabled(|| {
            let full_invl = self.full_invl.load(Ordering::Acquire);
            if full_invl {
                return false;
            }
            // In this case, we don't actually need to grab the lock
            if self.lock.swap(true, Ordering::Acquire) {
                return false;
            }
            let data = unsafe { self.data.get().as_mut().unwrap() };
            let ret = data.iter().all(Option::is_none);
            self.lock.store(false, Ordering::Release);
            ret
        })
    }

    pub fn complete(&self) {
        interrupt::with_disabled(|| {
            let mut iters = 0;
            while self.lock.swap(true, Ordering::Acquire) {
                iters += 1;
                if iters > 1000 {
                    log::warn!("TLB complete pause");
                }
                core::hint::spin_loop();
            }
            let full_invl = self.full_invl.swap(false, Ordering::SeqCst);
            if full_invl {
                let mut data = TlbInvData::new(0);
                data.set_global();
                data.set_full();
                data.do_invalidation();

                // Any other invalidations don't matter.
                self.reset();
                self.lock.store(false, Ordering::Release);
                return;
            }

            let data = unsafe { self.data.get().as_mut().unwrap() };
            for entry in data {
                if let Some(data) = entry.take() {
                    data.do_invalidation();
                    if data.full() && data.global() {
                        // Any other invalidations don't matter.
                        self.reset();
                        self.lock.store(false, Ordering::Release);
                        return;
                    }
                }
            }
            // explicit reset not needed because we've called take() on all entries
            self.lock.store(false, Ordering::Release);
        })
    }

    // must be called with the lock held
    fn reset(&self) {
        assert!(self.lock.load(Ordering::SeqCst));
        let data = unsafe { self.data.get().as_mut().unwrap() };
        for i in 0..NUM_TLB_SHOOTDOWN_ENTRIES {
            data[i] = None;
        }
    }
}
