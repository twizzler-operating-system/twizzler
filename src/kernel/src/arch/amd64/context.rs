use core::{
    arch::naked_asm,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use twizzler_abi::object::Protections;

use crate::{
    arch::{
        PhysAddr,
        memory::pagetables::{Entry, EntryFlags},
        processor::{CR3_IN_TRANSITION, NR_PCIDS, pcid_enabled},
    },
    memory::{
        VirtAddr,
        frame::{PHYS_LEVEL_LAYOUTS, get_frame},
        pagetables::{
            Consistency, ContiguousProvider, MapReader, Mapper, MappingCursor, MappingFlags,
            MappingSettings, PhysAddrProvider,
        },
        tracker::{FrameAllocFlags, FrameAllocator, alloc_frame, free_frame},
    },
    obj::pagetables::ObjectPageTable,
    once::Once,
    processor::{Processor, mp::with_each_active_processor},
    spinlock::{SpinLockGuard, Spinlock},
};

/// cr3[11:0], the PCID of the address space being loaded. Derived from [NR_PCIDS] rather than
/// spelled out, so the two cannot drift apart.
pub(crate) const PCID_MASK: u64 = NR_PCIDS as u64 - 1;
/// cr3[63]: load this root *without* invalidating the incoming PCID's entries.
const CR3_PCID_NOFLUSH: u64 = 1 << 63;
const PCID_ALLOC_WORDS: usize = NR_PCIDS / 64;

/// Allocation bitmap for PCIDs. PCID 0 is never handed out: it is the fallback for contexts that
/// couldn't get one, which never take the no-flush path and so may share it safely.
static PCID_ALLOC: [AtomicU64; PCID_ALLOC_WORDS] = [const { AtomicU64::new(0) }; PCID_ALLOC_WORDS];

/// Where the next [Pcid::alloc] scan starts, so allocation cycles through the space instead of
/// always handing back the lowest free PCID. Reuse is the expensive case -- it costs a valid-bit
/// clear on every processor, and then a flush on each of them the next time they run the new
/// owner -- so a freed PCID is better left alone until the cursor comes back around to it. Purely
/// an optimization: [PCID_ALLOC]'s CAS is what actually decides ownership, hence Relaxed
/// throughout, and a stale or wildly wrong hint costs at most one extra scan.
static PCID_HINT: AtomicUsize = AtomicUsize::new(1);

/// A PCID owned by one [ArchContext] for that context's whole lifetime, so that a given address
/// space has exactly one PCID everywhere. Zero means "none available"; see [PCID_ALLOC].
struct Pcid(u16);

impl Pcid {
    fn alloc() -> Self {
        if !pcid_enabled() {
            return Self(0);
        }
        let start = PCID_HINT.load(Ordering::Relaxed) % NR_PCIDS;
        // One extra word: the scan enters its first word partway in, so it has to come back to
        // that word at the end to see the bits it skipped on the way past.
        for n in 0..=PCID_ALLOC_WORDS {
            let i = (start / 64 + n) % PCID_ALLOC_WORDS;
            let word = &PCID_ALLOC[i];
            let reserved = if i == 0 { 1 } else { 0 };
            // Everything below the hint within its own word, on the first pass only.
            let skipped = if n == 0 {
                (1u64 << (start % 64)) - 1
            } else {
                0
            };
            loop {
                let cur = word.load(Ordering::Relaxed);
                let avail = !(cur | reserved | skipped);
                if avail == 0 {
                    break;
                }
                let bit = avail.trailing_zeros();
                if word
                    .compare_exchange_weak(
                        cur,
                        cur | (1 << bit),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    continue;
                }
                let pcid = (i * 64 + bit as usize) as u16;
                PCID_HINT.store((pcid as usize + 1) % NR_PCIDS, Ordering::Relaxed);
                // A context that has since been dropped may have run under this PCID, leaving
                // entries behind on some cpus. Nothing can be running it now, so clearing every
                // cpu's valid bit is enough -- each one will do a flushing cr3 load before it can
                // use the PCID again, and no IPI is needed to arrange that.
                with_each_active_processor(|p| {
                    p.arch.pcid_invalidate(pcid);
                });
                return Self(pcid);
            }
        }
        // Not fatal: PCID 0 just means this context switches the way it did before PCIDs.
        logln!("warning -- out of PCIDs, falling back to flush-on-switch for a context");
        Self(0)
    }
}

impl Drop for Pcid {
    fn drop(&mut self) {
        if self.0 == 0 {
            return;
        }
        let (word, bit) = (self.0 as usize / 64, self.0 as usize % 64);
        PCID_ALLOC[word].fetch_and(!(1 << bit), Ordering::AcqRel);
    }
}

pub struct ArchContext {
    pub target: ArchContextTarget,
    // Held for its Drop: the PCID is live for exactly as long as this context is.
    _pcid: Pcid,
    inner: Spinlock<Mapper>,
}

/// What gets loaded into cr3 for a context: its root page table address, with the context's PCID
/// in the low bits. Carrying the PCID here rather than alongside is what lets every consumer of a
/// target -- shootdown data, `ArchProcessor::active_cr3`, an object's invalidation list -- keep
/// comparing targets by equality, since cr3 reads back in exactly this format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct ArchContextTarget(u64);

impl ArchContextTarget {
    /// A target matching no real context, for invalidations that aren't tied to one.
    pub fn null() -> Self {
        Self(0)
    }

    pub fn paddr(&self) -> PhysAddr {
        PhysAddr::new(self.0 & !PCID_MASK).unwrap()
    }

    pub fn pcid(&self) -> u16 {
        (self.0 & PCID_MASK) as u16
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

static KERNEL_MAPPER: Once<Spinlock<Mapper>> = Once::new();

fn kernel_mapper() -> &'static Spinlock<Mapper> {
    KERNEL_MAPPER.call_once(|| {
        let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
        frame.inc_refcount();
        frame.set_pt(true);
        let mut m = Mapper::new(frame.start_address());
        for idx in 256..512 {
            let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
            frame.inc_refcount();
            frame.set_pt(true);
            m.set_top_level_table(
                idx,
                Entry::new(frame.start_address(), EntryFlags::intermediate()),
            );
        }
        Spinlock::new(m)
    })
}

impl Default for ArchContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchContext {
    pub fn new_kernel() -> Self {
        Self::new()
    }

    pub fn new() -> Self {
        let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
        frame.set_pt(true);
        frame.inc_refcount();
        let mut mapper = Mapper::new(frame.start_address());
        setup_mapper_with_kpages(&mut mapper);
        let pcid = Pcid::alloc();
        let root: u64 = mapper.root_address().into();
        let target = ArchContextTarget(root | pcid.0 as u64);
        Self {
            target,
            _pcid: pcid,
            inner: Spinlock::new(mapper),
        }
    }

    pub fn switch_to(&self, proc: Option<&Processor>) {
        unsafe { Self::switch_to_target(&self.target, proc) }
    }

    pub fn with_mapper<R>(&self, f: impl FnOnce(&mut Mapper) -> R) -> R {
        let mut inner = self.inner.lock();
        let result = f(&mut inner);
        result
    }

    /// Switch to a given set of page tables.
    ///
    /// `proc` should be the current processor, if TLS/the processor registry is up yet
    /// (it isn't during the very early boot switch from `memory::init()`) -- passing it
    /// explicitly keeps this low-level function from having to do its own TLS lookup.
    ///
    /// # Safety
    /// The specified target must be a root page table that will live as long as we are switched to
    /// it.
    pub unsafe fn switch_to_target(tgt: &ArchContextTarget, proc: Option<&Processor>) {
        // Interrupts must stay off from the CR3_IN_TRANSITION store through the cr3 write. An
        // invalidation racing us clears our PCID's valid bit and then, seeing CR3_IN_TRANSITION,
        // IPIs us and waits -- but a handler running *before* our cr3 write would find cr3 still
        // on the old root, do nothing, and let us go on to install a no-flush cr3 carrying the
        // very entries it came to kill. Deferring it past the write is what makes it land on the
        // right address space. Cheap when interrupts are already off, which is the common case.
        crate::interrupt::with_disabled(|| {
            // Advertise the transition *before* touching cr3. From the cr3 write onwards this
            // processor can cache entries for `tgt`, but it would still be advertising the old
            // root until the store below -- a shootdown for `tgt` landing in that window would
            // read the stale value, skip us, and free page-table pages out from under a live
            // walk. CR3_IN_TRANSITION matches every target, so we're covered for both roots
            // across the whole switch. The SeqCst store lowers to a locked op, which also keeps
            // it from being reordered after the cr3 write.
            if let Some(proc) = proc {
                proc.arch
                    .active_cr3
                    .store(CR3_IN_TRANSITION, Ordering::SeqCst);
            }
            // cr3 reads back as root|pcid (bit 63 always reads zero), so this compares like for
            // like. Everything below, the claim on the PCID included, is inside the branch: when
            // we are already running this context there is no flush to back a claim with, and an
            // invalidation may have just cleared the bit with its IPI still pending behind our
            // disabled interrupts. Setting it here would then let a later switch into this
            // context skip the flush that invalidation is counting on. Leaving the bit alone is
            // never wrong -- at worst it is conservatively clear and costs one extra flush.
            if tgt.0 != unsafe { x86::controlregs::cr3() } {
                // `proc` is None exactly while this cpu is still short of `processor::init`,
                // which is where CR4.PCIDE goes on -- and cr3[4:3] mean PWT/PCD until it does.
                // Targets carry a PCID from the moment they are built, which is earlier than
                // that, so mask it off rather than writing it into control bits.
                let mut val = if proc.is_some() {
                    tgt.0
                } else {
                    tgt.0 & !PCID_MASK
                };
                // Claiming the PCID before the write, not after: if we get to skip the flush it
                // is because someone had already set the bit, and if we don't, the flush this
                // write performs is exactly what makes the bit we just set true.
                if let Some(proc) = proc {
                    // Counted here rather than around the whole function: only this branch writes
                    // cr3, and only a write could ever have flushed. Per-cpu and Relaxed because
                    // this is the switch path -- a shared counter's locked op and contended
                    // cacheline would cost more than the flush being measured.
                    if tgt.pcid() != 0 && proc.arch.pcid_test_and_set(tgt.pcid()) {
                        val |= CR3_PCID_NOFLUSH;
                        proc.stats
                            .aspace_switch_noflush
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        proc.stats
                            .aspace_switch_flush
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
                unsafe {
                    x86::controlregs::cr3_write(val);
                }
            }
            if let Some(proc) = proc {
                proc.arch.active_cr3.store(tgt.0, Ordering::Release);
            }
        })
    }

    fn lock_with_consist(&self, cursor: MappingCursor) -> (Consistency, SpinLockGuard<'_, Mapper>) {
        let consist = if cursor.start().is_kernel() {
            Consistency::new_full_global()
        } else {
            Consistency::new(self.target)
        };
        let guard = self.inner.lock();
        (consist, guard)
    }

    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        fa: &mut FrameAllocator,
    ) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);

        guard.map(cursor, phys, &mut consist, fa).unwrap();
        consist.finish_send();
        drop(guard);
        consist.into_deferred().run_all();
    }

    pub fn object_map(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
        fa: &mut FrameAllocator,
    ) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        let took_ref = guard
            .object_map(cursor, object_tables, settings, &mut consist, fa)
            .unwrap();
        consist.finish_send();
        drop(guard);
        consist.into_deferred().run_all();
        // Only count a map if we actually took a new reference, so that the single dec_map_count
        // done on unmap stays symmetric.
        if took_ref {
            object_tables.inc_map_count();
        }
    }

    /// Whether this context already holds the object-table entry [`Self::ensure_object_mapped`]
    /// would install.
    ///
    /// Split out so a caller can ask before precharging a frame allocator: the entry is normally
    /// already there -- a couple of percent of the faults that reach this install one -- and the
    /// precharge cannot happen under the lock this takes, so it has to be decided in advance.
    pub fn is_object_mapped(&self, cursor: MappingCursor, settings: MappingSettings) -> bool {
        self.inner.lock().is_object_mapped(cursor, settings)
    }

    pub fn ensure_object_mapped(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
        fa: &mut FrameAllocator,
    ) -> bool {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        if !guard.is_object_mapped(cursor, settings) {
            let took_ref = guard
                .object_map(cursor, object_tables, settings, &mut consist, fa)
                .unwrap();
            consist.finish_send();
            drop(guard);
            consist.into_deferred().run_all();
            if took_ref {
                object_tables.inc_map_count();
            }
            true
        } else {
            false
        }
    }

    pub fn change(
        &self,
        cursor: MappingCursor,
        settings: &MappingSettings,
        fa: &mut FrameAllocator,
    ) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        guard.change(cursor, settings, &mut consist, fa).unwrap();
        consist.finish_send();
        drop(guard);
        consist.into_deferred().run_all();
    }

    pub fn unmap(&self, cursor: MappingCursor, fa: &mut FrameAllocator) -> bool {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        let r = guard.unmap(cursor, &mut consist, fa, &mut None).unwrap();
        consist.finish_send();
        drop(guard);
        consist.into_deferred().run_all();
        r
    }

    /// Unmap an object mapping, returning true if doing so released this context's reference to
    /// `obj_table` -- the table [Self::object_map] installed for that object. That, and not "did we
    /// unmap anything", is the question the object's map count needs answered: the entry at this
    /// address can belong to a different object, in which case the count must not move.
    pub fn unmap_object(
        &self,
        cursor: MappingCursor,
        obj_table: Option<PhysAddr>,
        fa: &mut FrameAllocator,
    ) -> bool {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        let mut released = None;
        let _ = guard
            .unmap(cursor, &mut consist, fa, &mut released)
            .unwrap();
        consist.finish_send();
        drop(guard);
        consist.into_deferred().run_all();
        obj_table.is_some() && released == obj_table
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        let r = if cursor.start().is_kernel() {
            f(kernel_mapper().lock().readmap(cursor))
        } else {
            f(self.inner.lock().readmap(cursor))
        };
        r
    }
}

#[unsafe(naked)]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_trap() {
    naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "xor rdi, rdi",
        "xor rsi, rsi",
        "xor rax, rax",
        "syscall",
        "__here:",
        "jmp __here",
        "pop rbp",
        "ret"
    );
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn trap_entry() {
    panic!("hit trap entry");
}

fn setup_mapper_with_kpages(mapper: &mut Mapper) {
    let km = kernel_mapper().lock();
    let mut fa = FrameAllocator::new(
        FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
        PHYS_LEVEL_LAYOUTS[0],
    );
    for idx in 256..512 {
        mapper.set_top_level_table(idx, km.get_top_level_table(idx));
    }
    let frame = fa.try_allocate().unwrap();
    let mut z = ContiguousProvider::new(
        frame.start_address(),
        0x1000,
        MappingSettings::new(
            Protections::READ | Protections::EXEC,
            twizzler_abi::device::CacheType::WriteBack,
            MappingFlags::USER,
        ),
    );
    let mut consist = Consistency::new_full_global();
    mapper
        .map(
            MappingCursor::new(VirtAddr::new(0).unwrap(), 0x1000),
            &mut z,
            &mut consist,
            &mut fa,
        )
        .unwrap();
    consist.tlb_mut().finish();
    consist.into_deferred().run_all();
    let start = trampoline_trap as *const u8;
    let len = 0x100;
    #[allow(invalid_null_arguments)]
    let dest = frame.start_address().kernel_vaddr().as_mut_ptr::<u8>();
    unsafe { dest.copy_from(start, len) };
}

/// Report if any processor is still running `target`, whose page tables are about to be freed and
/// whose PCID is about to go back to the pool.
///
/// This is the quiescence claim two separate mechanisms rest on, neither of which can survive it
/// being wrong: [`ArchContext::drop`] frees the root page table outright, and [`Pcid::alloc`] hands
/// the PCID to a new context on the strength of "nothing can be running it now" -- it only clears
/// every cpu's valid bit, which does nothing for a cpu that has this root in cr3 *right now*. The
/// argument for it is that a `SecurityContext` has no detach, so its last reference drops only when
/// the last thread attached to it is dropped, which is after that thread is off-cpu -- and
/// `Thread::switch_thread` switches context-less threads to the kernel context precisely so an idle
/// cpu is not left sitting on a user root. That argument was never checked anywhere. This checks
/// it.
///
/// Reports rather than panics: it runs from a `Drop`, and panicking here would take out the very
/// teardown path most likely to hold the bug, losing the transcript that names the cpu and the
/// target. Both failures it detects are unsurvivable anyway, so a run that trips it fails shortly
/// after on its own -- but with a message that does not name this cause, which is the whole point.
///
/// amd64 only: aarch64 keeps no per-cpu record of the root it is running, so there is nothing to
/// check against there.
fn check_quiesced(target: ArchContextTarget) {
    // A processor partway through a switch publishes CR3_IN_TRANSITION, which matches every target
    // (see `should_target`) and so cannot be told apart from ours. That window is a few
    // instructions wide and runs with interrupts off, so re-reading resolves it; one that is still
    // in transition afterwards is reported as indeterminate rather than spun on forever.
    const TRANSITION_RETRIES: usize = 10000;
    with_each_active_processor(|p| {
        let mut active = p.arch.active_cr3.load(Ordering::Acquire);
        let mut tries = 0;
        while active == CR3_IN_TRANSITION && tries < TRANSITION_RETRIES {
            core::hint::spin_loop();
            active = p.arch.active_cr3.load(Ordering::Acquire);
            tries += 1;
        }
        if active == target.0 {
            emerglogln!(
                "context teardown: cpu {} is still running the context being dropped (cr3 {:#x}, pcid {}) -- its root is about to be freed",
                p.id,
                target.0,
                target.pcid(),
            );
        } else if active == CR3_IN_TRANSITION {
            emerglogln!(
                "context teardown: cpu {} is still mid-switch after {} reads, so whether it is switching into the context being dropped (cr3 {:#x}) is unknown",
                p.id,
                TRANSITION_RETRIES,
                target.0,
            );
        }
    });
}

impl Drop for ArchContext {
    fn drop(&mut self) {
        // Before the unmap, not just before the root free: the walk below frees page-table pages
        // too, so a cpu still on these tables is already unsafe by then.
        check_quiesced(self.target);
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Unmap all user memory to clear any allocated page tables.
        self.unmap(
            MappingCursor::new(
                VirtAddr::start_user_memory(),
                VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
            ),
            &mut fa,
        );
        // Manually free the root.
        if let Some(frame) = get_frame(self.inner.lock().root_address()) {
            frame.set_pt(false);
            if frame.dec_refcount() == 0 {
                free_frame(frame);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor::mp::current_processor;

    /// PCIDs must be exclusive while live, and returned to the pool once their owner is gone --
    /// both are what let a PCID stand in for an address space identity.
    #[twizzler_kernel_macros::kernel_test]
    fn test_pcid_alloc_unique_and_freed() {
        if !pcid_enabled() {
            return;
        }
        let a = Pcid::alloc();
        let b = Pcid::alloc();
        assert_ne!(a.0, 0);
        assert_ne!(b.0, 0);
        assert_ne!(a.0, b.0);

        // Assert on the pool rather than on what a subsequent alloc returns: another cpu building
        // a context would take the freed PCID first and make that racy.
        let freed = b.0;
        drop(b);
        let (word, bit) = (freed as usize / 64, freed as usize % 64);
        assert_eq!(PCID_ALLOC[word].load(Ordering::Relaxed) & (1u64 << bit), 0);
        let (word, bit) = (a.0 as usize / 64, a.0 as usize % 64);
        assert_ne!(PCID_ALLOC[word].load(Ordering::Relaxed) & (1u64 << bit), 0);
    }

    /// The valid bit is the whole no-flush decision: set means "may skip the flush", and it must
    /// only report already-set for a PCID nobody has invalidated since.
    #[twizzler_kernel_macros::kernel_test]
    fn test_pcid_valid_bit() {
        if !pcid_enabled() {
            return;
        }
        // Allocating it keeps any real context from owning this PCID while we poke at it.
        let pcid = Pcid::alloc();
        assert_ne!(pcid.0, 0);
        let proc = current_processor();

        proc.arch.pcid_invalidate(pcid.0);
        assert!(!proc.arch.pcid_test_and_set(pcid.0));
        assert!(proc.arch.pcid_test_and_set(pcid.0));
        proc.arch.pcid_invalidate(pcid.0);
        assert!(!proc.arch.pcid_test_and_set(pcid.0));

        // Neighbouring PCIDs share a bitmap word, so an off-by-one in the bit math shows up here.
        let other = Pcid::alloc();
        proc.arch.pcid_invalidate(other.0);
        assert!(proc.arch.pcid_test_and_set(pcid.0));
        assert!(!proc.arch.pcid_test_and_set(other.0));

        proc.arch.pcid_invalidate(pcid.0);
        proc.arch.pcid_invalidate(other.0);
    }

    /// A freshly allocated PCID must not be usable no-flush anywhere: whoever had it before may
    /// have left entries behind.
    #[twizzler_kernel_macros::kernel_test]
    fn test_pcid_alloc_clears_valid_bit() {
        if !pcid_enabled() {
            return;
        }
        let proc = current_processor();
        let first = Pcid::alloc();
        let id = first.0;
        drop(first);
        // Stand in for entries the previous owner left behind on this cpu.
        proc.arch.pcid_test_and_set(id);

        // Aim the round-robin cursor back at the PCID we just freed. Without this the allocator
        // would sweep the other 4095 first and this test would assert nothing.
        PCID_HINT.store(id as usize, Ordering::Relaxed);
        let second = Pcid::alloc();
        // Another cpu building a context could still have taken `id` between the drop and the
        // store; the property only means anything for the PCID we actually got back.
        if second.0 == id {
            assert!(!proc.arch.pcid_test_and_set(id));
        }
        proc.arch.pcid_invalidate(second.0);
        proc.arch.pcid_invalidate(id);
    }

    /// A freed PCID goes to the back of the queue rather than straight back out. Handing it
    /// straight back is what costs a valid-bit clear on every cpu plus a flush on each of them
    /// later, so the allocator sweeps the space instead.
    #[twizzler_kernel_macros::kernel_test]
    fn test_pcid_alloc_rotates() {
        if !pcid_enabled() {
            return;
        }
        let first = Pcid::alloc();
        let id = first.0;
        drop(first);
        // A concurrent allocator can only push the cursor further past `id`, never back onto it --
        // that would take a full wrap of all 4095 -- so this holds without a guard.
        let second = Pcid::alloc();
        assert_ne!(second.0, id);
    }

    /// A target round-trips its root and PCID, and hands `paddr()` back a bare root.
    #[twizzler_kernel_macros::kernel_test]
    fn test_target_packing() {
        let root = PhysAddr::new(0x1234_5000).unwrap();
        let tgt = ArchContextTarget(u64::from(root) | 0xabc);
        assert_eq!(tgt.pcid(), 0xabc);
        assert_eq!(tgt.paddr(), root);
        assert_eq!(ArchContextTarget::null().pcid(), 0);
    }
}
