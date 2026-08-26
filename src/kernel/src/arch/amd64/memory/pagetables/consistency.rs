use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use x86::controlregs::Cr4;

use crate::{
    arch::{
        address::VirtAddr,
        context::{ArchContextTarget, PCID_MASK},
        interrupt::TLB_SHOOTDOWN_VECTOR,
        processor::CR3_IN_TRANSITION,
    },
    interrupt::{self, Destination},
    memory::pagetables::{
        MappingCursor, TlbOrigin, tlb_shootdown_inc_count, tlb_wait_record, trace_tlb_invalidation,
        trace_tlb_shootdown,
    },
    processor::{
        Processor,
        mp::{current_processor, with_each_active_processor},
        sched::CpuSet,
        spin_wait_until, tls_ready,
    },
    thread::current_thread_ref,
};

/// A/B knob for acknowledging a shootdown without taking the target's lock.
///
/// `is_finished` used to take the target cpu's [`TlbShootdownInfo::lock`], so a sender spinning in
/// [`PendingShootdown::do_wait`] issued a cli and a `lock xchg` on the very cache line that target
/// must acquire in `complete` to acknowledge -- ~100 times per pause, since `spin_wait_until` polls
/// the condition that often between pauses. A failed swap also read as "not finished", so colliding
/// with the acknowledger lengthened the wait that was waiting on it.
///
/// The knock-on is likely the larger half and is not on the shootdown path at all: `complete` gets
/// the same lock-free early-out, and it runs from `spin_wait_iteration` -- i.e. inside *every*
/// contended spinlock acquisition in the kernel -- where the common case is an empty queue that
/// otherwise costs a cli and a locked RMW to discover.
///
/// `false` restores the lock-taking readers. [`TlbShootdownInfo::has_work`] is maintained in both
/// arms so that only the read path differs; its stores are to a line the writer already holds
/// exclusively under the lock.
pub const TLB_LOCKFREE_ACK: bool = true;

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

    /// The PCID of the address space being invalidated, or 0 if this invalidation isn't tied to
    /// one (or the context in question is on the no-PCID fallback).
    fn pcid(&self) -> u16 {
        (self.target_cr3 & PCID_MASK) as u16
    }

    fn instructions(&self) -> &[InvInstruction] {
        &self.instructions[0..(self.len as usize)]
    }

    /// Whether `p` needs to be sent (and waited on for) this invalidation. A processor
    /// whose active address space doesn't match our target is guaranteed to flush before
    /// it can use stale entries for that address space: it will switch into our target
    /// later, and [ArchTlbMgr::finish] has already cleared its valid bit for our PCID, so
    /// that switch loads cr3 without CR3_PCID_NOFLUSH -- by then the underlying page-table
    /// write that triggered this invalidation has already happened, so it'll walk fresh,
    /// correct PTEs. (Without PCIDs every `mov cr3` flushes, and the same holds trivially.)
    /// Global invalidations always go to every processor regardless, matching the
    /// receiver-side check in `do_invalidation`. A processor midway through a page-table
    /// switch publishes `CR3_IN_TRANSITION`, which matches here, because it may hold
    /// entries for either root.
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

    /// Re-assert this processor's no-flush claim on the address space this invalidation names,
    /// because we have just made our entries for it correct.
    ///
    /// [ArchTlbMgr::finish_send] revokes every other processor's claim before it sends, including
    /// the processors it is about to hand a *precise* invalidation to. That is safe -- and the
    /// revoke has to stay, since it is what covers a processor the sender then decides not to
    /// target -- but it charges those processors a full flush on their next switch into an
    /// address space whose entries this invalidation has just repaired. Measured on
    /// debug-kvm-smp4: `aspace_flush_revoked` (2925/boot) tracks `aspace_switch_flush` (2949) to
    /// within 1%, and precise-send targets account for ~86% of it.
    ///
    /// What makes this safe is that it asserts a fact about *this* processor, established
    /// locally: we have just executed every instruction this invalidation carried, or flushed the
    /// PCID outright. It says nothing about any other processor and takes no lock.
    ///
    /// Pairs with [Self::drop_claim_here], and neither is correct without the other -- see there.
    fn reassert_claim_here(&self) {
        let pcid = self.pcid();
        if pcid == 0 || !tls_ready() {
            return;
        }
        let proc = current_processor();
        // Only count the transitions. An already-set bit is the sender-was-us case and the
        // repeat-invalidation case, neither of which averted a flush.
        if !proc.arch.pcid_test_and_set(pcid) {
            proc.stats
                .aspace_claim_reasserted
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Drop this processor's claim on the address space this invalidation names, because the
    /// invalidation turned out not to apply to us.
    ///
    /// Without [Self::reassert_claim_here] this would be pure redundancy -- the sender already
    /// revoked us before sending, so the bit is normally clear and the RMW does nothing. With it,
    /// it is load-bearing, and this is the one interleaving that needs it: a *different* sender
    /// revokes our claim and is still between its revoke and its IPI when we reassert on some
    /// other invalidation for the same PCID. If we then switched away and back we would take
    /// `CR3_PCID_NOFLUSH` while holding entries that sender is invalidating. Its IPI still
    /// arrives -- it is spinning for our acknowledgement -- and lands here, on the arm where our
    /// cr3 no longer matches, and this drops the claim again. Every processor a sender targets
    /// therefore either applies the invalidation or gives up its claim; there is no third
    /// outcome, which is the property the sender's unconditional revoke provided before.
    fn drop_claim_here(&self) {
        let pcid = self.pcid();
        if pcid == 0 || !tls_ready() {
            return;
        }
        let proc = current_processor();
        if proc.arch.pcid_invalidate(pcid) {
            proc.stats
                .aspace_claim_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
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
        // Re-enabling these needs emerglogln, not logln: this runs from inside `GenericSpinlock`'s
        // spin, where taking the console lock deadlocks a cpu against itself.
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
        use crate::memory::pagetables::invl_census::{self, Outcome};
        // If none of the commands are global, and it's targeting a different set of
        // page tables than is active, then we can ignore it.
        let ours = our_cr3 == self.target();
        if !ours && !self.global() {
            invl_census::record(Outcome::Skipped);
            self.drop_claim_here();
            return;
        }

        if self.full() {
            invl_census::record(if self.global() {
                Outcome::FullGlobal
            } else {
                Outcome::FullLocal
            });
            if self.global() {
                tlb_global_inv();
            } else {
                tlb_non_global_inv();
            }
            // A global full flush clears every PCID, so our entries for the target are gone
            // whether or not we are running it. `tlb_non_global_inv` is a cr3 reload and so
            // reaches only the current PCID, which is the target exactly when `ours`.
            if ours || self.global() {
                self.reassert_claim_here();
            }
            return;
        }

        invl_census::record(Outcome::Precise(self.instructions().len()));
        for inst in self.instructions() {
            inst.execute();
        }
        // `invlpg` acts on the current PCID plus global entries, so a precise invalidation leaves
        // the target's entries correct only when the target is what we are running. The global
        // arm reaching here (a GLOBAL-flagged instruction, without FULL) has not touched the
        // target PCID's own entries at all.
        if ours {
            self.reassert_claim_here();
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

/// Issue `clflush` for page-table entries as they are written.
///
/// **Off**, because on this architecture nothing reads those lines out of memory. Three facts,
/// each checkable rather than argued:
///
/// 1. **The x86 page-table walker is coherent with the data caches.** It snoops, so an entry
///    written and left dirty in L1 is seen by the next walk. This is the whole of what
///    `update_entry`'s flush was doing on the hot path -- and it pays twice, once for the flush and
///    again for the miss the *next* walk takes on the line it just evicted.
/// 2. **No page-table frame is in persistent memory.** `MemoryRegionKind` has exactly three
///    variants -- `UsableRam`, `Reserved`, `BootloaderReserved` -- and `Table::populate` allocates
///    through the ordinary frame allocator, so the durability motivation for flushing a page table
///    has no instance in this tree. **This is the precondition to re-check**: if a persistent
///    region kind is ever added and page tables can land in it, this must come back on for those
///    tables.
/// 3. **The one ordering argument that names `clflush`** -- `Table::do_cow_copy`'s comment about
///    the downgrade loop being ordered before the entry update below -- **does not need it on
///    x86.** x86-TSO does not reorder stores with stores, so the loop's writes are already visible
///    before the later entry write, to other cpus and to their page walkers alike. The `clflush`
///    leg of that argument was redundant on this architecture. It is *not* redundant on aarch64,
///    where the walker may not be coherent and the equivalent manager issues `dc cvac; dsb ishst;
///    isb` -- so this const is deliberately amd64-local rather than a change to the generic
///    `add_cache_line` call sites, which aarch64 still needs.
///
/// Kept as a switch rather than deleted so the cost is measurable in both directions and so fact 2
/// has somewhere to be re-read when it stops being true.
const PT_CLFLUSH: bool = false;

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
        if !PT_CLFLUSH {
            return;
        }
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
    /// Statistics only -- see [TlbOrigin]. Defaults to `Arch`; the object side marks its own.
    origin: TlbOrigin,
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
    pub fn new(target: ArchContextTarget) -> Self {
        let this = Self {
            data: TlbInvData::new(target.raw()),
            origin: TlbOrigin::Arch,
        };
        assert!(!this.data.has_invalidations());
        this
    }

    pub fn set_origin(&mut self, origin: TlbOrigin) {
        self.origin = origin;
    }

    pub fn new_full_global() -> Self {
        let mut this = Self::new(ArchContextTarget::null());
        this.set_full_global();
        this
    }

    pub fn set_full_global(&mut self) {
        self.data.set_full();
        self.data.set_global();
    }

    /// Invalidate everything for this manager's target, without going machine-wide.
    ///
    /// For a change too broad to express as addresses: `invlpg` takes one page, so a downgrade
    /// applied across a whole sub-table -- where at level > 1 each entry covers 512 further pages
    /// -- has no precise encoding. See `do_cow_copy`.
    pub fn set_full(&mut self) {
        self.data.set_full();
    }

    pub fn is_full(&self) -> bool {
        self.data.full()
    }

    pub fn set_target(&mut self, target: ArchContextTarget) {
        self.data.target_cr3 = target.raw();
    }

    pub fn reset(&mut self) {
        self.data.reset();
    }

    pub fn apply_offset_from_map(&self, map: &MappingCursor) -> Self {
        let data = self.data.apply_offset(map);
        Self {
            data,
            origin: self.origin,
        }
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

    /// Targets at or below which the shootdown IPI is sent one cpu at a time instead of broadcast.
    ///
    /// Sized small deliberately: the cost being avoided is per *bystander* (a spurious vector plus,
    /// under virtualization, a vm exit), and the cost being paid is per *target* (an ICR write and
    /// a delivery-status spin, serially). Both scale, so the win is widest when the target set is a
    /// small fraction of the machine -- which after PCID revocation is the normal case. Above this
    /// the serial ICR writes start to cost more than the interrupts they save, and one broadcast is
    /// the better trade.
    const MAX_TARGETED_IPIS: usize = 4;

    /// Execute all queued invalidations, and wait for them to be acknowledged.
    pub fn finish(&mut self) {
        self.finish_send().wait();
    }

    /// Distribute the queued invalidations and apply them locally, without waiting for remote
    /// processors to acknowledge. The returned token must be waited on before the memory this
    /// invalidation protects is reused; dropping it waits.
    ///
    /// Split out from [Self::finish] so a caller can drop its page-table lock across the wait. Only
    /// the wait moves: the revoke, the fence, the target selection and the send all stay here, so
    /// the ordering argument below holds exactly as it did when this was one function.
    pub fn finish_send(&mut self) -> PendingShootdown {
        use crate::memory::context::virtmem::unmapprofile as up;
        if !tls_ready() {
            self.reset();
            return PendingShootdown::none();
        }
        if !self.data.has_invalidations() {
            return PendingShootdown::none();
        }

        let ct = current_thread_ref();
        let _guard = ct.as_ref().map(|ct| ct.enter_critical());
        // We definitely don't want to reschedule to a different CPU while doing this.
        let proc = current_processor();

        // Single-processor fast path. With no other cpu in existence there is no claim to revoke,
        // no target to select, no IPI to send, and nothing to wait for -- the local invalidation
        // is the entire job. This skips two full-processor sweeps and the ordering fence on every
        // invalidation on a one-cpu system. It cannot widen to a "count == 0" check on smp>1: there
        // the revoke below is load-bearing for every cpu we then decide *not* to target (it makes
        // them flush on their next switch into this address space), so it must run even when the
        // send targets nobody.
        if crate::processor::mp::is_single_processor() {
            self.data.do_invalidation();
            drop(_guard);
            self.data.reset();
            return PendingShootdown::none();
        }

        let mut count = 0;
        // Revoke every *other* processor's right to switch into this address space without
        // flushing. A processor we skip below relies on that flush to shed the entries we are
        // invalidating; one we do IPI gets a precise invalidation instead, but revoking its bit
        // too costs only a redundant flush later and keeps this independent of who we end up
        // targeting. We keep our own bit only when we are running the target ourselves, since
        // then `do_invalidation` below invalidates precisely for us. (This is FreeBSD's
        // pmap_invalidate_preipi_pcid, minus the generation counter: our PCIDs are per address
        // space, not per cpu, so there is nothing to re-allocate.)
        //
        // Racing a `switch_to_target` for the same PCID, exactly one of two things happens, and
        // the RMWs on the bitmap word are what decide which. Either its `fetch_or` follows our
        // `fetch_and` in that word's modification order, so it reads the bit clear and flushes;
        // or it precedes ours, in which case our `fetch_and` reads from its release (RMWs join
        // the release sequence, so intervening traffic on other bits in the same word doesn't
        // break this) and its earlier CR3_IN_TRANSITION store therefore happens-before our load
        // of active_cr3 -- so we see it and IPI it. Both sides' AcqRel is load-bearing: the same
        // edge, read the other way, is what publishes our page-table writes to a processor that
        // flushes instead of being IPI'd.
        let t_st = up::start();
        let pcid = self.data.pcid();
        if pcid != 0 {
            let ours = unsafe { x86::controlregs::cr3() } == self.data.target();
            with_each_active_processor(|p| {
                if !(ours && p.id == proc.id) {
                    // Counted against the cpu losing the claim rather than the one issuing it, and
                    // only when there was a claim to lose: what the number is for is comparing
                    // "flushes I was forced into" against "flushes I got to skip", and both are
                    // per-victim. Most calls here clear an already-clear bit and cost nothing.
                    if p.arch.pcid_invalidate(pcid) {
                        p.stats.aspace_flush_revoked.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        // Our caller's page-table writes must be visible to any processor that we then decide
        // *not* to target. Those writes and the `active_cr3` loads below form a store->load
        // pair, the one reordering x86 permits, so without this fence we could observe a
        // processor's pre-switch cr3 while it observes our pre-unmap PTEs -- and we would skip
        // it. Pairs with the SeqCst store in `ArchContext::switch_to_target`.
        up::record(up::Stage::SendRevoke, t_st);
        let t_st = up::start();
        core::sync::atomic::fence(Ordering::SeqCst);
        // Distribute the invalidation commands, recording exactly who we sent to. `should_target`
        // reads each processor's active cr3, which can change underneath us, so the wait below has
        // to use the set we actually sent to rather than re-evaluating the predicate against a
        // cr3 that has since moved on.
        let mut targets = CpuSet::empty();
        let mut others = 0;
        with_each_active_processor(|p| {
            if p.id == proc.id {
                return;
            }
            others += 1;
            if self.data.should_target(p) {
                p.arch.tlb_shootdown_info.insert(self.data.clone());
                targets.insert(p.id);
                count += 1;
            }
        });
        tlb_shootdown_inc_count(count, self.origin, self.data.full() && self.data.global());
        up::record(up::Stage::SendTarget, t_st);
        let t_st = up::start();
        if count > 0 {
            trace_tlb_shootdown();
            // Send the IPI, and then do local invalidations.
            //
            // `targets` was already computed precisely; sending to it rather than broadcasting is
            // just spending that. A broadcast makes every untargeted cpu take the vector and run
            // `tlb_shootdown_handler`, and `do_invalidation` then discards it outright for not
            // matching its cr3 -- which under virtualization is a vm exit bought for nothing. The
            // trade is that `raw_send_ipi` writes the ICR and spins for delivery status, so N
            // targeted sends pay that serially against one for a broadcast.
            //
            // Most shootdowns sit well under the threshold, because the PCID revocation above has
            // already dropped every cpu not currently running this address space: `count` is the
            // number executing in it *right now*, which outside the monitor and the pager is zero
            // or one. `count == others` short-circuits to the broadcast because then there is no
            // bystander left to spare and the singles would be pure overhead.
            if count <= Self::MAX_TARGETED_IPIS && count < others {
                with_each_active_processor(|p| {
                    if targets.contains(p.id) {
                        super::super::super::apic::send_ipi(
                            Destination::Single(p.id),
                            TLB_SHOOTDOWN_VECTOR,
                        );
                    }
                });
            } else {
                super::super::super::apic::send_ipi(Destination::AllButSelf, TLB_SHOOTDOWN_VECTOR);
            }
        }
        up::record(up::Stage::SendIpi, t_st);
        let t_st = up::start();
        trace_tlb_invalidation();
        self.data.do_invalidation();
        up::record(up::Stage::SendLocal, t_st);

        // Released before the wait, not after it. It is load-bearing for everything above -- the
        // revoke, the target selection and the local invalidation all have to happen on one
        // processor -- but nothing in the wait assumes it is still on the processor that sent.
        // Targets drain from the IPI whether or not we are on-cpu, `is_finished` reads *their*
        // state, and landing on a processor that is itself in `targets` resolves itself, since the
        // resend below is delivered locally and its handler drains. Releasing it here is what lets
        // a token be held across a sleep, which the object page tables need.
        drop(_guard);
        self.data.reset();
        // No remote target: the revoke above already covered every other cpu (they flush on their
        // next switch into this address space) and the local invalidation is done, so there is
        // nothing to wait for. Return an empty token rather than one carrying a target set whose
        // wait is a no-op.
        if count == 0 {
            return PendingShootdown::none();
        }
        PendingShootdown {
            targets,
            count,
            from: proc.id,
            origin: self.origin,
        }
    }
}

/// A shootdown that has been sent but not yet acknowledged by its targets.
///
/// Dropping one waits, so a caller that ignores it gets the old behavior rather than a correctness
/// bug. That is deliberate: the guarantee this represents -- no processor still holds a stale entry
/// for the range -- is what makes it safe to free the unmapped frames, and losing it silently would
/// be a use-after-free rather than a slowdown.
#[must_use = "the shootdown must be waited for before the memory it protects is reused"]
pub struct PendingShootdown {
    targets: CpuSet,
    count: usize,
    /// The processor that sent, for the stall message. Recorded rather than read at wait time
    /// because this may no longer be running there.
    from: u32,
    /// Statistics only: which page tables this came out of, so the wait can be attributed. See
    /// [tlb_wait_record] -- object-origin wait time is what the object page tables would move out
    /// from under their mutex, and is the number that decides whether that is worth doing.
    origin: TlbOrigin,
}

impl PendingShootdown {
    /// Nothing was sent, so there is nothing to wait for.
    pub fn none() -> Self {
        Self {
            targets: CpuSet::empty(),
            count: 0,
            from: 0,
            origin: TlbOrigin::Arch,
        }
    }

    pub fn wait(mut self) {
        self.do_wait();
    }

    /// Take on another token's obligation, so that several sends can be waited for once.
    ///
    /// Waiting on the union is not weaker than waiting on each: `is_finished` reports on all of a
    /// processor's slots rather than on a particular sender's entry, so a processor that has
    /// drained has drained everything either token put there.
    pub fn absorb(&mut self, mut other: Self) {
        if self.count == 0 {
            // Ours is a `none()`, whose `origin` and `from` are placeholders; take the real ones.
            // `from` matters: both object-side callers accumulate onto a `none()`, so without this
            // every object-origin stall would report having been sent by cpu 0 -- in the one
            // message anyone reads while investigating a stall.
            self.origin = other.origin;
            self.from = other.from;
        }
        // With several real sends absorbed, `origin` and `from` keep the first. Both are reporting
        // only, and in practice every absorbed set shares an origin.
        self.targets.union_with(&other.targets);
        self.count += other.count;
        // We hold the obligation now; its Drop must not wait for it a second time.
        other.count = 0;
    }

    /// Idempotent, so that [Self::wait] and the `Drop` that backs it up cannot double-wait.
    fn do_wait(&mut self) {
        if self.count == 0 {
            return;
        }
        let start = crate::instant::Instant::now();
        // Wait for every targeted processor to report that it is done. This wait must not be
        // bounded: the caller frees the unmapped frames -- page table pages included -- once it
        // returns, so giving up early lets a processor that still holds stale entries walk recycled
        // memory. It cannot deadlock, because spin_wait_until services our own incoming shootdowns
        // on every pass.
        //
        // Resend periodically rather than on every pass: a processor that had interrupts disabled
        // when the broadcast went out needs another nudge, but hammering its APIC only delays the
        // acknowledgement we're waiting for.
        const RESEND_INTERVAL: usize = 4096;
        const WARN_INTERVAL: usize = 1 << 22;
        with_each_active_processor(|p| {
            if !self.targets.contains(p.id) {
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
                            self.from,
                            p.id,
                            iters
                        );
                    }
                    None
                },
                || {},
            );
        });
        tlb_wait_record(self.origin, crate::instant::Instant::now() - start);
        self.count = 0;
    }
}

impl Drop for PendingShootdown {
    fn drop(&mut self) {
        self.do_wait();
    }
}

impl Drop for ArchTlbMgr {
    fn drop(&mut self) {
        // Nothing was ever enqueued, so `finish` has nothing to send: `finish_send` returns
        // `PendingShootdown::none()` at its `has_invalidations` guard and the `wait` on it returns
        // at its `count == 0` guard. Returning here is the same no-op without building and
        // destroying a 144-byte token to express it -- and this Drop runs once per `map_page`,
        // from inside `Consistency::into_deferred`, on a path where nothing is ever enqueued.
        if !self.data.has_invalidations() {
            return;
        }
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
    /// Whether this cpu still owes anyone a drain, readable without the lock. See
    /// [`TLB_LOCKFREE_ACK`] for why that matters.
    ///
    /// Set by `insert` with Release *after* the slot write, so a reader that sees `true` sees the
    /// slot. Cleared by `complete` with Release only once every invalidation has been applied and
    /// while the lock is still held, so a reader that sees `false` happens-after those
    /// invalidations -- which is exactly the guarantee that makes freeing the unmapped frames safe.
    ///
    /// Read together with `full_invl`, never alone. For slots the lock serializes set against
    /// clear, so this flag alone would do; the `full_invl` bail is the exception, because it runs
    /// having *failed* to take the lock, and a `complete` finishing concurrently can land its clear
    /// between that path's two stores. No store order fixes that -- only reading both does, which
    /// is what the old lock-taking reader did by accident.
    has_work: AtomicBool,
}

impl TlbShootdownInfo {
    pub fn new() -> Self {
        Self {
            data: UnsafeCell::new([None, None, None, None]),
            lock: AtomicBool::new(false),
            full_invl: AtomicBool::new(false),
            has_work: AtomicBool::new(false),
        }
    }

    pub fn insert(&self, new_data: TlbInvData) {
        interrupt::with_disabled(|| {
            let mut iters = 0;
            while self.lock.swap(true, Ordering::Acquire) {
                iters += 1;
                if iters >= 100 {
                    log::warn!("failed to insert tlb shootdown info -- setting full_invl");
                    self.has_work.store(true, Ordering::Release);
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
                    self.has_work.store(true, Ordering::Release);
                    self.lock.store(false, Ordering::Release);
                    return;
                }
            }
            // Try to find a slot with the same target_cr3
            for entry in data.iter_mut() {
                // Unwrap-Ok: we know that all slots are Some from the first loop.
                if entry.as_ref().unwrap().target() == new_data.target() {
                    entry.as_mut().unwrap().merge(new_data);
                    self.has_work.store(true, Ordering::Release);
                    self.lock.store(false, Ordering::Release);
                    return;
                }
            }
            // Choose the 0'th entry because if this makes it a full or global entry, we want to be
            // able to exit the handling loop early.
            // Unwrap-Ok: we know that all slots are Some from the first loop.
            data[0].as_mut().unwrap().merge(new_data);
            self.has_work.store(true, Ordering::Release);
            self.lock.store(false, Ordering::Release);
        })
    }

    /// Whether this cpu has applied everything sent to it. Called from a *remote* cpu's wait spin,
    /// so under [`TLB_LOCKFREE_ACK`] it is two plain loads and takes neither the lock nor a cli.
    pub fn is_finished(&self) -> bool {
        if TLB_LOCKFREE_ACK {
            return !self.has_work.load(Ordering::Acquire)
                && !self.full_invl.load(Ordering::Acquire);
        }
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
        // Nothing published, nothing to drain. This runs on every pass of `spin_wait_iteration`,
        // i.e. from inside every contended spinlock acquisition in the kernel, where the
        // overwhelmingly common case is an empty queue -- previously paying a cli and a locked RMW
        // to find that out. An `insert` still mid-flight is not a miss: it has not sent its IPI
        // yet, so nobody is waiting on us for it, and it publishes before releasing the lock.
        if TLB_LOCKFREE_ACK
            && !self.has_work.load(Ordering::Acquire)
            && !self.full_invl.load(Ordering::Acquire)
        {
            return;
        }
        interrupt::with_disabled(|| {
            let mut iters = 0;
            while self.lock.swap(true, Ordering::Acquire) {
                iters += 1;
                // emerglogln, not log::warn, and once rather than per iteration. This runs from
                // two contexts that must not take the console spinlock: the IPI handler, which can
                // interrupt a cpu already holding it, and `spin_wait_iteration` from inside
                // `GenericSpinlock::lock`'s spin -- where, if the lock being waited for *is* the
                // console lock, taking a second ticket on it deadlocks the cpu against itself.
                if iters == 1001 {
                    emerglogln!("TLB complete pause");
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
                self.has_work.store(false, Ordering::Release);
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
                        self.has_work.store(false, Ordering::Release);
                        self.lock.store(false, Ordering::Release);
                        return;
                    }
                }
            }
            // explicit reset not needed because we've called take() on all entries.
            // Cleared last, and only here: every release of `has_work` promises a waiting sender
            // that the invalidations above have already been applied on this cpu.
            self.has_work.store(false, Ordering::Release);
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
