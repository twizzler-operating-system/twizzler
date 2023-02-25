use crate::{
    arch::memory::pagetables::{Entry, EntryFlags},
    memory::{
        frame::{alloc_frame, PhysicalFrameFlags},
        pagetables::{
            DeferredUnmappingOps, MapReader, Mapper, MappingCursor, MappingSettings,
            PhysAddrProvider,
        },
    },
    mutex::Mutex,
    spinlock::Spinlock,
};

pub struct ArchContextInner {
    mapper: Mapper,
}

pub struct ArchContext {
    target: u64,
    inner: Mutex<ArchContextInner>,
}

lazy_static::lazy_static! {
    static ref KERNEL_MAPPER: Spinlock<Mapper> = {
        let mut m = Mapper::new(alloc_frame(PhysicalFrameFlags::ZEROED).start_address());
        for idx in 256..512 {
            m.set_top_level_table(idx, Entry::new(alloc_frame(PhysicalFrameFlags::ZEROED).start_address(), EntryFlags::intermediate()));
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
    pub fn new_kernel() -> Self {
        Self::new()
    }

    pub fn new() -> Self {
        let inner = ArchContextInner::new();
        let target = inner.mapper.root_address().into();
        Self {
            target,
            inner: Mutex::new(inner),
        }
    }

    #[allow(named_asm_labels)]
    pub fn switch_to(&self) {
        unsafe {
            x86::controlregs::cr3_write(self.target);
        }
    }

    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        if cursor.start().is_kernel() {
            KERNEL_MAPPER.lock().map(cursor, phys, settings);
        } else {
            self.inner.lock().map(cursor, phys, settings);
        }
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        if cursor.start().is_kernel() {
            KERNEL_MAPPER.lock().change(cursor, settings);
        } else {
            self.inner.lock().change(cursor, settings);
        }
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let ops = if cursor.start().is_kernel() {
            KERNEL_MAPPER.lock().unmap(cursor)
        } else {
            self.inner.lock().unmap(cursor)
        };
        ops.run_all();
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        let r = if cursor.start().is_kernel() {
            f(KERNEL_MAPPER.lock().readmap(cursor))
        } else {
            f(self.inner.lock().mapper.readmap(cursor))
        };
        r
    }
}

impl ArchContextInner {
    fn new() -> Self {
        let mut mapper = Mapper::new(alloc_frame(PhysicalFrameFlags::ZEROED).start_address());
        let km = KERNEL_MAPPER.lock();
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
