use arm64::registers::{TTBR0_EL1, TTBR1_EL1};

use crate::{
    arch::memory::pagetables::{Entry, EntryFlags, Table},
    memory::{
        frame::get_frame,
        pagetables::{
            DeferredUnmappingOps, MapReader, Mapper, MappingCursor, MappingSettings,
            PhysAddrProvider,
        },
        tracker::{alloc_frame, free_frame, FrameAllocFlags},
        PhysAddr,
    },
    mutex::Mutex,
    once::Once,
    spinlock::Spinlock,
    VirtAddr,
};

// this does not need to be pub
pub struct ArchContextInner {
    // we have a single mapper that covers one part of the address space
    mapper: Mapper,
}

pub struct ArchContext {
    pub target: ArchContextTarget,
    inner: Mutex<ArchContextInner>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
// TODO: can we get the kernel tables elsewhere?
pub struct ArchContextTarget(PhysAddr);

// default kernel mapper that is shared among all kernel instances of ArchContext
static KERNEL_MAPPER: Once<(Spinlock<Mapper>, PhysAddr)> = Once::new();

fn kernel_mapper() -> &'static (Spinlock<Mapper>, PhysAddr) {
    KERNEL_MAPPER.call_once(|| {
        let mut m = Mapper::new(
            // allocate a new physical page frame to hold the
            // data for the page table root
            alloc_frame(FrameAllocFlags::ZEROED).start_address(),
        );
        // initialize half of the page table entries
        for idx in (Table::PAGE_TABLE_ENTRIES / 2)..Table::PAGE_TABLE_ENTRIES {
            // write out PT entries for a top level table
            // whose entries point to another zeroed page
            m.set_top_level_table(
                idx,
                Entry::new(
                    alloc_frame(FrameAllocFlags::ZEROED).start_address(),
                    // intermediate here means another page table
                    EntryFlags::intermediate(),
                ),
            );
        }
        let root = m.root_address();
        (Spinlock::new(m), root)
    })
}

impl Default for ArchContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchContext {
    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Self {
        let inner = ArchContextInner::new();
        let target = ArchContextTarget(inner.mapper.root_address());
        Self {
            target,
            inner: Mutex::new(inner),
        }
    }

    pub fn new() -> Self {
        Self::new_kernel()
    }

    pub fn switch_to(&self) {
        unsafe {
            Self::switch_to_target(&self.target);
        }
    }

    #[allow(named_asm_labels)]
    /// Switch to a target context.
    ///
    /// # Safety
    /// This function must be called with a target that comes from an ArchContext that lives long
    /// enough.
    pub unsafe fn switch_to_target(tgt: &ArchContextTarget) {
        // TODO: If the incoming target is already the current user table, this should be a no-op.
        // Also, we don't need to set the kernel tables each time.
        // write TTBR1
        TTBR1_EL1.set_baddr(kernel_mapper().1.raw());
        // write TTBR0
        TTBR0_EL1.set_baddr(tgt.0.raw());
        core::arch::asm!(
            // ensure that all previous instructions have completed
            "isb",
            // invalidate all tlb entries (locally)
            "tlbi vmalle1",
            // ensure tlb invalidation completes
            "dsb nsh",
            // ensure dsb instruction completes
            "isb",
        );
    }

    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        // decide if this goes into the global kernel mappings, or
        // the local per-context mappings
        if cursor.start().is_kernel() {
            // upper half addresses go to TTBR1_EL1
            kernel_mapper().0.lock().map(cursor, phys, settings);
        } else {
            // lower half addresses go to TTBR0_EL1
            self.inner.lock().map(cursor, phys, settings);
        }
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        if cursor.start().is_kernel() {
            kernel_mapper().0.lock().change(cursor, settings);
        } else {
            self.inner.lock().change(cursor, settings);
        }
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let ops = if cursor.start().is_kernel() {
            kernel_mapper().0.lock().unmap(cursor)
        } else {
            self.inner.lock().unmap(cursor)
        };
        ops.run_all();
    }

    pub fn readmap<R>(&self, _cursor: MappingCursor, _f: impl Fn(MapReader) -> R) -> R {
        todo!("readmap")
    }
}

impl ArchContextInner {
    fn new() -> Self {
        // we need to create a new mapper object by allocating
        // some memory for the page table.
        let mapper = Mapper::new(alloc_frame(FrameAllocFlags::ZEROED).start_address());
        Self { mapper }
    }

    fn map(
        &mut self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        self.mapper.map(cursor, phys, settings);
    }

    fn change(&mut self, cursor: MappingCursor, settings: &MappingSettings) {
        self.mapper.change(cursor, settings);
    }

    fn unmap(&mut self, cursor: MappingCursor) -> DeferredUnmappingOps {
        self.mapper.unmap(cursor)
    }
}

impl Drop for ArchContextInner {
    fn drop(&mut self) {
        // Unmap all user memory to clear any allocated page tables.
        self.mapper
            .unmap(MappingCursor::new(
                VirtAddr::start_user_memory(),
                VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
            ))
            .run_all();
        // Manually free the root.
        if let Some(frame) = get_frame(self.mapper.root_address()) {
            free_frame(frame);
        }
    }
}
