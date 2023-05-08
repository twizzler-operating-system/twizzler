use crate::{
    // arch::memory::pagetables::{Entry, EntryFlags},
    memory::{
        // frame::{alloc_frame, PhysicalFrameFlags},
        pagetables::{
            DeferredUnmappingOps, MapReader, MappingCursor, MappingSettings,
            PhysAddrProvider,
        },
    },
    // mutex::Mutex,
    // spinlock::Spinlock,
};

// this does not need to be pub
pub struct ArchContextInner;

pub struct ArchContext;

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
        Self {}
    }

    #[allow(named_asm_labels)]
    pub fn switch_to(&self) {
        todo!("switch_to")
    }

    pub fn map(
        &self,
        _cursor: MappingCursor,
        _phys: &mut impl PhysAddrProvider,
        _settings: &MappingSettings,
    ) {
        todo!("map")
    }

    pub fn change(&self, _cursor: MappingCursor, _settings: &MappingSettings) {
        todo!("change")
    }

    pub fn unmap(&self, _cursor: MappingCursor) {
        todo!("unmap")
    }

    pub fn readmap<R>(&self, _cursor: MappingCursor, _f: impl Fn(MapReader) -> R) -> R {
        todo!("readmap")
    }
}

impl ArchContextInner {
    fn new() -> Self {
        todo!()
    }

    fn map(
        &mut self,
        _cursor: MappingCursor,
        _phys: &mut impl PhysAddrProvider,
        _settings: &MappingSettings,
    ) {
        todo!()
    }

    fn change(&mut self, _cursor: MappingCursor, _settings: &MappingSettings) {
        todo!()
    }

    fn unmap(&mut self, _cursor: MappingCursor) -> DeferredUnmappingOps {
        todo!()
    }
}
