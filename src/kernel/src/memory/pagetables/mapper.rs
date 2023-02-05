use crate::arch::{address::PhysAddr, memory::pagetables::Table};

use super::{consistency::Consistency, MapInfo, MappingCursor, MappingSettings, PhysAddrProvider};

/// Manager for a set of page tables. This is the primary interface for manipulating a set of page tables.
pub struct Mapper {
    root: PhysAddr,
    start_level: usize,
}

impl Mapper {
    /// Construct a new set of tables from an existing root page.
    pub fn new(root: PhysAddr) -> Self {
        Self {
            root,
            start_level: Table::top_level(),
        }
    }

    pub(super) fn root_mut(&mut self) -> &mut Table {
        unsafe { &mut *(self.root.kernel_vaddr().as_mut_ptr::<Table>()) }
    }

    pub(super) fn root(&self) -> &Table {
        unsafe { &*(self.root.kernel_vaddr().as_ptr::<Table>()) }
    }

    /// Map a set of physical pages into the tables with the provided settings.
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

    /// Unmap a region from the page tables.
    pub fn unmap(&mut self, cursor: MappingCursor) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.unmap(&mut consist, cursor, level);
    }

    /// Change a region to use new mapping settings.
    pub fn change(&mut self, cursor: MappingCursor, settings: &MappingSettings) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.change(&mut consist, cursor, level, settings);
    }

    /// Read the map of a single address (the start of the cursor).
    pub(super) fn do_read_map(&self, cursor: &MappingCursor) -> Option<MapInfo> {
        let level = self.start_level;
        let root = self.root();
        root.readmap(cursor, level)
    }
}
