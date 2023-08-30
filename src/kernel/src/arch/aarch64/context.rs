use arm64::registers::{TTBR0_EL1, TTBR1_EL1};

use crate::{
    arch::memory::pagetables::{Entry, EntryFlags, Table},
    memory::{
        frame::{alloc_frame, PhysicalFrameFlags},
        pagetables::{
            DeferredUnmappingOps, Mapper, MapReader, MappingCursor, MappingSettings,
            PhysAddrProvider,
        },
        PhysAddr,
    },
    mutex::Mutex,
    spinlock::Spinlock,
};

// this does not need to be pub
pub struct ArchContextInner {
    // we have a single mapper that covers one part of the address space
    mapper: Mapper,
}
pub struct ArchContext {
    kernel: u64, // TODO: do we always need a copy?
    user: PhysAddr,
    inner: Mutex<ArchContextInner>,
}

// default kernel mapper that is shared among all kernel instances of ArchContext
lazy_static::lazy_static! {
    static ref KERNEL_MAPPER: Spinlock<Mapper> = {
        let mut m = Mapper::new(
            // allocate a new physical page frame to hold the
            // data for the page table root
            alloc_frame(PhysicalFrameFlags::ZEROED).start_address()
        );
        // initialize half of the page table entries
        for idx in (Table::PAGE_TABLE_ENTRIES/2)..Table::PAGE_TABLE_ENTRIES {
            // write out PT entries for a top level table
            // whose entries point to another zeroed page
            m.set_top_level_table(idx, 
                Entry::new(
                    alloc_frame(PhysicalFrameFlags::ZEROED)
                        .start_address(), 
                    // intermediate here means another page table
                    EntryFlags::intermediate()
                )
            );
        }
        Spinlock::new(m)
    };
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
        Self {
            kernel: KERNEL_MAPPER.lock().root_address().raw(),
            user: inner.mapper.root_address(),
            inner: Mutex::new(inner),
        }
    }

    pub fn new() -> Self {
        Self::new_kernel()
    }

    #[allow(named_asm_labels)]
    pub fn switch_to(&self) {
        // TODO: make sure the TTBR1_EL1 switch only happens once
        // write TTBR1
        TTBR1_EL1.set_baddr(self.kernel);
        // write TTBR0
        TTBR0_EL1.set_baddr(self.user.raw());
        unsafe { 
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
            KERNEL_MAPPER.lock().map(cursor, phys, settings);
        } else {
            // lower half addresses go to TTBR0_EL1
            self.inner.lock().map(cursor, phys, settings);
        }
    }

    pub fn change(&self, _cursor: MappingCursor, _settings: &MappingSettings) {
        // TODO: change page table entry
    }

    pub fn unmap(&self, _cursor: MappingCursor) {
        // TODO: actually unmap pages
    }

    pub fn readmap<R>(&self, _cursor: MappingCursor, _f: impl Fn(MapReader) -> R) -> R {
        todo!("readmap")
    }
}

impl ArchContextInner {
    fn new() -> Self {
        // we need to create a new mapper object by allocating 
        // some memory for the page table.
        let mapper = Mapper::new(
            alloc_frame(PhysicalFrameFlags::ZEROED).start_address()
        );
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

    fn change(&mut self, _cursor: MappingCursor, _settings: &MappingSettings) {
        todo!()
    }

    fn unmap(&mut self, _cursor: MappingCursor) -> DeferredUnmappingOps {
        todo!()
    }
}
