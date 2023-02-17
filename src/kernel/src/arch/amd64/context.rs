use core::panic;

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
    pub fn new_kernel() -> Self {
        let inner = ArchContextInner::new_kernel();
        let target = inner.mapper.root_address().into();
        Self {
            target,
            inner: Mutex::new(inner),
        }
    }

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
    fn new_kernel() -> Self {
        Self { mapper: todo!() }
    }

    fn map(
        &mut self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        panic!("todo: should check to see if we are in kernel mem, and use kernel mapper");
        self.mapper.map(cursor, phys, settings);
    }

    fn change(&mut self, cursor: MappingCursor, settings: &MappingSettings) {
        self.mapper.change(cursor, settings);
    }

    fn unmap(&mut self, cursor: MappingCursor) -> DeferredUnmappingOps {
        self.mapper.unmap(cursor)
    }
}
