use crate::arch::address::PhysAddr;

use super::{
    consistency::Consistency, MapInfo, MappingCursor, MappingSettings, PhysAddrProvider, Table,
};

pub struct Mapper {
    root: PhysAddr,
    start_level: usize,
}

impl Mapper {
    pub fn new(root: PhysAddr) -> Self {
        Self {
            root,
            start_level: 3, /* TODO: arch-dep */
        }
    }

    pub fn root_mut(&mut self) -> &mut Table {
        unsafe { &mut *(self.root.kernel_vaddr().as_mut_ptr::<Table>()) }
    }

    pub fn root(&self) -> &Table {
        unsafe { &*(self.root.kernel_vaddr().as_ptr::<Table>()) }
    }

    pub fn map(
        &mut self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.map(&mut consist, cursor, level, phys, settings);
    }

    pub fn unmap(&mut self, cursor: MappingCursor) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.unmap(&mut consist, cursor, level);
    }

    pub fn change(&mut self, cursor: MappingCursor, settings: &MappingSettings) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.change(&mut consist, cursor, level, settings);
    }

    pub(super) fn do_read_map(&self, cursor: &MappingCursor) -> Option<MapInfo> {
        let level = self.start_level;
        let root = self.root();
        root.readmap(cursor, level)
    }
}
