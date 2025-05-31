use super::{
    consistency::{Consistency, DeferredUnmappingOps},
    MapInfo, MappingCursor, MappingSettings, PhysAddrProvider,
};
use crate::arch::{
    address::PhysAddr,
    memory::pagetables::{Entry, Table},
};

/// Manager for a set of page tables. This is the primary interface for manipulating a set of page
/// tables.
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

    /// Create a mapper for the current table.
    ///
    /// # Safety
    /// This function is VERY UNSAFE because it allows RW and WW conflicts. It
    /// must only be used during initialization of the system.
    pub unsafe fn current() -> Mapper {
        Self::new(Table::current())
    }

    pub(super) fn root_mut(&mut self) -> &mut Table {
        unsafe { &mut *(self.root.kernel_vaddr().as_mut_ptr::<Table>()) }
    }

    pub(super) fn root(&self) -> &Table {
        unsafe { &*(self.root.kernel_vaddr().as_ptr::<Table>()) }
    }

    /// Set a top level table to a direct value. Useful for creating large regions of global memory
    /// (like the kernel's vaddr memory range). Does not perform any consistency operations.
    pub fn set_top_level_table(&mut self, index: usize, entry: Entry) {
        let root = self.root_mut();
        let was_present = root[index].is_present();
        let count = root.read_count();
        root[index] = entry;
        if was_present && !entry.is_present() {
            root.set_count(count - 1)
        } else if !was_present && entry.is_present() {
            root.set_count(count + 1)
        } else {
            root.set_count(count)
        }
    }

    /// Get a top level table entry's value. Useful for cloning large regions during creation (e.g.
    /// the kernel's memory region).
    pub fn get_top_level_table(&self, index: usize) -> Entry {
        let root = self.root();
        root[index]
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
    ) -> Result<(), DeferredUnmappingOps> {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        if root.map(&mut consist, cursor, level, phys).is_none() {
            drop(consist);
            let mut consist = Consistency::new(self.root);
            let root = self.root_mut();
            root.unmap(&mut consist, cursor, level);
            Err(consist.into_deferred())
        } else {
            Ok(())
        }
    }

    #[must_use]
    /// Unmap a region from the page tables. The deferred operations must be run, and must be run
    /// AFTER unlocking any page table locks.
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

    /// Read the map of a single address (the start of the cursor). If there is a mapping at the
    /// specified location, return the mapping information. Otherwise, return Err with a length
    /// that specifies how much the cursor may advance before calling this function again to
    /// check for a new mapping.
    pub(super) fn do_read_map(&self, cursor: &MappingCursor) -> Result<MapInfo, usize> {
        let level = self.start_level;
        let root = self.root();
        root.readmap(cursor, level)
    }
}
