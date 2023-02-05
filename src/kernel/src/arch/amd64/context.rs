use crate::{
    memory::pagetables::{Mapper, MappingCursor, MappingSettings, PhysAddrProvider},
    mutex::Mutex,
};

pub struct ArchContextInner {
    mapper: Mapper,
}

pub struct ArchContext {
    inner: Mutex<ArchContextInner>,
}

impl ArchContext {
    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        self.inner.lock().map(cursor, phys, settings);
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        self.inner.lock().unmap(cursor);
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

    fn unmap(&mut self, cursor: MappingCursor) {
        self.mapper.unmap(cursor);
    }
}
