use arm64::registers::{TTBR0_EL1, TTBR1_EL1};

use crate::{
    VirtAddr,
    arch::memory::pagetables::{Entry, EntryFlags, Table},
    memory::{
        PhysAddr,
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame},
        pagetables::{
            Consistency, MapReader, Mapper, MappingCursor, MappingSettings, PhysAddrProvider,
        },
        tracker::{FrameAllocFlags, FrameAllocator, alloc_frame, free_frame},
    },
    obj::pagetables::ObjectPageTable,
    once::Once,
    processor::Processor,
    spinlock::{SpinLockGuard, Spinlock},
};

/// A context's TTBR0 root. Kernel addresses are never mapped here: they live in the shared TTBR1
/// tables behind [`kernel_mapper`], which every context loads alongside its own root.
pub struct ArchContext {
    pub target: ArchContextTarget,
    inner: Spinlock<Mapper>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct ArchContextTarget(pub PhysAddr);

impl ArchContextTarget {
    /// A target matching no real context, for invalidations that aren't tied to one.
    pub fn null() -> Self {
        Self(PhysAddr::new(0).unwrap())
    }

    pub fn paddr(&self) -> PhysAddr {
        self.0
    }

    pub fn raw(&self) -> u64 {
        self.0.raw()
    }
}

/// The mapper and its root, the latter kept outside the lock so a context switch can load TTBR1
/// without taking it.
static KERNEL_MAPPER: Once<(Spinlock<Mapper>, PhysAddr)> = Once::new();

fn kernel_mapper() -> &'static (Spinlock<Mapper>, PhysAddr) {
    KERNEL_MAPPER.call_once(|| {
        let mut m = Mapper::new(new_table_frame().start_address());
        for idx in (Table::PAGE_TABLE_ENTRIES / 2)..Table::PAGE_TABLE_ENTRIES {
            m.set_top_level_table(
                idx,
                Entry::new(
                    new_table_frame().start_address(),
                    EntryFlags::intermediate(),
                ),
            );
        }
        let root = m.root_address();
        (Spinlock::new(m), root)
    })
}

/// Cross-check the MMU against the software walk of the shared kernel tables, over the kernel
/// image and the 1 GiB of the physical map holding a freshly allocated frame. Pages checked.
#[cfg(test)]
pub fn check_kernel_map(samples: usize) -> usize {
    let frame = alloc_frame(FrameAllocFlags::KERNEL);
    let gib = 1u64 << 30;
    let ram = PhysAddr::new(frame.start_address().raw() & !(gib - 1)).unwrap();
    free_frame(frame);
    let text = VirtAddr::new((check_kernel_map as fn(usize) -> usize) as u64 & !0xfff).unwrap();
    let mapper = kernel_mapper().0.lock();
    [
        MappingCursor::new(text, 16 << 20),
        MappingCursor::new(ram.kernel_vaddr(), gib as usize),
    ]
    .into_iter()
    .map(|c| super::memory::check_map(&mapper, c, samples / 2))
    .sum()
}

fn new_table_frame() -> FrameRef {
    let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
    frame.set_pt(true);
    frame.inc_refcount();
    frame
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
        let mapper = Mapper::new(new_table_frame().start_address());
        Self {
            target: ArchContextTarget(mapper.root_address()),
            inner: Spinlock::new(mapper),
        }
    }

    pub fn switch_to(&self, proc: Option<&Processor>) {
        unsafe { Self::switch_to_target(&self.target, proc) }
    }

    pub fn with_mapper<R>(&self, f: impl FnOnce(&mut Mapper) -> R) -> R {
        f(&mut self.inner.lock())
    }

    /// Switch to a target context.
    ///
    /// `proc` is unused on this architecture (the amd64 backend uses it to track per-processor
    /// active-context state for targeted TLB shootdown); accepted so callers shared with amd64
    /// compile.
    ///
    /// # Safety
    /// `tgt` must come from an `ArchContext` that outlives the switch.
    pub unsafe fn switch_to_target(tgt: &ArchContextTarget, _proc: Option<&Processor>) {
        // Same tables, same TLB contents: changes to them are invalidated by broadcast `tlbi`
        // when made, so only a real change of root needs the local flush (there are no ASIDs).
        let kroot = kernel_mapper().1.raw();
        if TTBR0_EL1.get_baddr() == tgt.0.raw() && TTBR1_EL1.get_baddr() == kroot {
            return;
        }
        TTBR1_EL1.set_baddr(kroot);
        TTBR0_EL1.set_baddr(tgt.0.raw());
        unsafe {
            core::arch::asm!("isb", "tlbi vmalle1", "dsb nsh", "isb");
        }
    }

    /// The tables `cursor` lives in: kernel addresses go to the shared TTBR1 tables, which are
    /// visible everywhere and so invalidated everywhere.
    fn lock_with_consist(&self, cursor: MappingCursor) -> (Consistency, SpinLockGuard<'_, Mapper>) {
        if cursor.start().is_kernel() {
            (Consistency::new_full_global(), kernel_mapper().0.lock())
        } else {
            (Consistency::new(self.target), self.inner.lock())
        }
    }

    fn mapper_for(&self, cursor: MappingCursor) -> SpinLockGuard<'_, Mapper> {
        if cursor.start().is_kernel() {
            kernel_mapper().0.lock()
        } else {
            self.inner.lock()
        }
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

    /// See the amd64 counterpart: whether a new reference to `object_tables` was taken.
    #[must_use]
    pub fn object_map(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
        fa: &mut FrameAllocator,
    ) -> bool {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        let took_ref = guard
            .object_map(cursor, object_tables, settings, &mut consist, fa)
            .unwrap();
        consist.finish_send();
        drop(guard);
        consist.into_deferred().run_all();
        took_ref
    }

    pub fn is_object_mapped(&self, cursor: MappingCursor, settings: MappingSettings) -> bool {
        self.mapper_for(cursor).is_object_mapped(cursor, settings)
    }

    /// `None` if the mapping was already present; otherwise `Some(took_ref)` as for
    /// [`Self::object_map`].
    #[must_use]
    pub fn ensure_object_mapped(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
        fa: &mut FrameAllocator,
    ) -> Option<bool> {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        if guard.is_object_mapped(cursor, settings) {
            return None;
        }
        let took_ref = guard
            .object_map(cursor, object_tables, settings, &mut consist, fa)
            .unwrap();
        consist.finish_send();
        drop(guard);
        consist.into_deferred().run_all();
        Some(took_ref)
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

    /// Unmap an object mapping, returning whether doing so released this context's reference to
    /// `obj_table`. The amd64 counterpart explains the three detached cases.
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
        match (released, obj_table) {
            (Some(r), Some(t)) if r == t => true,
            (Some(_), Some(_)) => {
                crate::memory::context::virtmem::unmap_census::record_foreign();
                false
            }
            (Some(_), None) => {
                crate::memory::context::virtmem::unmap_census::record_unverified();
                true
            }
            (None, _) => false,
        }
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        f(self.mapper_for(cursor).readmap(cursor))
    }
}

impl Drop for ArchContext {
    fn drop(&mut self) {
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
