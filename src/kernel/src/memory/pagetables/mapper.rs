use crate::arch::{address::PhysAddr, memory::pagetables::Table};

use super::{
    consistency::{Consistency, DeferredUnmappingOps},
    MapInfo, MappingCursor, MappingSettings, PhysAddrProvider,
};

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

    /// Get the root of the page tables as a physical address.
    pub fn root_address(&self) -> PhysAddr {
        self.root
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

    #[must_use]
    /// Unmap a region from the page tables. The deferred operations must be run, and must be run AFTER unlocking any
    /// page table locks.
    pub fn unmap(&mut self, cursor: MappingCursor) -> DeferredUnmappingOps {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.unmap(&mut consist, cursor, level);
        consist.into_deferred()
    }

    /// Change a region to use new mapping settings.
    pub fn change(&mut self, cursor: MappingCursor, settings: &MappingSettings) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.change(&mut consist, cursor, level, settings);
    }

    /// Read the map of a single address (the start of the cursor). If there is a mapping at the specified location,
    /// return the mapping information. Otherwise, return Err with a length that specifies how much the cursor may
    /// advance before calling this function again to check for a new mapping.
    pub(super) fn do_read_map(&self, cursor: &MappingCursor) -> Result<MapInfo, usize> {
        let level = self.start_level;
        let root = self.root();
        root.readmap(cursor, level)
    }
}
