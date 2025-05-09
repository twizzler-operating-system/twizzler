use crate::{
    arch::memory::pagetables::{Entry, EntryFlags},
    memory::{
        frame::get_frame,
        pagetables::{
            DeferredUnmappingOps, MapReader, Mapper, MappingCursor, MappingSettings,
            PhysAddrProvider,
        },
        tracker::{alloc_frame, free_frame, FrameAllocFlags},
        VirtAddr,
    },
    mutex::Mutex,
    once::Once,
    spinlock::Spinlock,
};

pub struct ArchContextInner {
    mapper: Mapper,
}

pub struct ArchContext {
    pub target: ArchContextTarget,
    inner: Mutex<ArchContextInner>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct ArchContextTarget(u64);

static KERNEL_MAPPER: Once<Spinlock<Mapper>> = Once::new();

fn kernel_mapper() -> &'static Spinlock<Mapper> {
    KERNEL_MAPPER.call_once(|| {
        let mut m = Mapper::new(
            alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL).start_address(),
        );
        for idx in 256..512 {
            m.set_top_level_table(
                idx,
                Entry::new(
                    alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL).start_address(),
                    EntryFlags::intermediate(),
                ),
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
        let inner = ArchContextInner::new();
        let target = ArchContextTarget(inner.mapper.root_address().into());
        Self {
            target,
            inner: Mutex::new(inner),
        }
    }

    pub fn switch_to(&self) {
        unsafe { Self::switch_to_target(&self.target) }
    }

    /// Switch to a given set of page tables.
    ///
    /// # Safety
    /// The specified target must be a root page table that will live as long as we are switched to
    /// it.
    pub unsafe fn switch_to_target(tgt: &ArchContextTarget) {
        unsafe {
            if tgt.0 != x86::controlregs::cr3() {
                x86::controlregs::cr3_write(tgt.0);
            }
        }
    }

    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        let ops = if cursor.start().is_kernel() {
            kernel_mapper().lock().map(cursor, phys, settings)
        } else {
            self.inner.lock().map(cursor, phys, settings)
        };
        if let Err(ops) = ops {
            ops.run_all();
        }
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        if cursor.start().is_kernel() {
            kernel_mapper().lock().change(cursor, settings);
        } else {
            self.inner.lock().change(cursor, settings);
        }
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let ops = if cursor.start().is_kernel() {
            kernel_mapper().lock().unmap(cursor)
        } else {
            self.inner.lock().unmap(cursor)
        };
        ops.run_all();
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        let r = if cursor.start().is_kernel() {
            f(kernel_mapper().lock().readmap(cursor))
        } else {
            f(self.inner.lock().mapper.readmap(cursor))
        };
        r
    }
}

impl ArchContextInner {
    fn new() -> Self {
        let mut mapper = Mapper::new(
            alloc_frame(
                FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
            )
            .start_address(),
        );
        let km = kernel_mapper().lock();
        for idx in 256..512 {
            mapper.set_top_level_table(idx, km.get_top_level_table(idx));
        }
        Self { mapper }
    }

    fn map(
        &mut self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) -> Result<(), DeferredUnmappingOps> {
        self.mapper.map(cursor, phys, settings)
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
