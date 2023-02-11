use crate::{
    memory::pagetables::{
        DeferredUnmappingOps, Mapper, MappingCursor, MappingSettings, PhysAddrProvider,
    },
    mutex::Mutex,
};

pub struct ArchContextInner {
    mapper: Mapper,
}

pub struct ArchContext {
    target: u64,
    inner: Mutex<ArchContextInner>,
}

impl ArchContext {
    pub fn switch_to(&self) {
        todo!()
    }

    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        self.inner.lock().map(cursor, phys, settings);
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        self.inner.lock().change(cursor, settings);
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let ops = { self.inner.lock().unmap(cursor) };
        ops.run_all();
    }
}

impl ArchContextInner {
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
