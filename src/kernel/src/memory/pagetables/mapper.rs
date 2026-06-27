use twizzler_rt_abi::error::{ResourceError, TwzError};

use super::{
    MapInfo, MappingCursor, MappingSettings, PhysAddrProvider,
    consistency::{Consistency, DeferredUnmappingOps},
};
use crate::{
    arch::{
        VirtAddr,
        address::PhysAddr,
        memory::pagetables::{Entry, EntryFlags, Table},
    },
    obj::pagetables::ObjectPageTable,
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
        mut consist: Consistency,
    ) -> Result<(), DeferredUnmappingOps> {
        let level = self.start_level;
        let root = self.root_mut();
        if root.map(&mut consist, cursor, level, phys).is_none() {
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

    pub fn set_start_level(&mut self, start_level: usize) {
        self.start_level = start_level;
    }

    pub fn start_level(&self) -> usize {
        self.start_level
    }

    pub fn object_map(
        &mut self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
    ) -> DeferredUnmappingOps {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        object_tables.with_mapper(|mapper| {
            root.object_map(&mut consist, cursor, level, mapper);
            consist.into_deferred()
        })
    }

    pub fn get_table_addr(&mut self, level: usize) -> PhysAddr {
        log::info!(
            "get_table_addr called with level {} (start_level {})",
            level,
            self.start_level
        );
        let start_level = self.start_level;
        let mut table_phys = self.root_address();
        let mut table = self.root_mut();
        for _ in 0..(start_level - level) {
            if table[0].is_present() && level > 0 && table[0].is_huge() {
                panic!("todo: get_table_addr: huge page at level {}!", level);
            }
            // TODO: unwrap
            table.populate(0, EntryFlags::intermediate()).unwrap();
            table_phys = table[0].table_addr();
            table = table.next_table_mut(0).unwrap();
        }

        table_phys
    }

    pub fn split_to_level(&mut self, addr: VirtAddr, level: usize) -> Result<(), TwzError> {
        let mut consist = Consistency::new(self.root);
        let start_level = self.start_level;
        let root = self.root_mut();
        root.split_to_level(&mut consist, addr, start_level, level)
            .ok_or(ResourceError::OutOfMemory)?;
        consist.into_deferred().run_all();
        Ok(())
    }

    pub fn setup_cow_range(
        &mut self,
        dest: &mut Mapper,
        mut src_cursor: MappingCursor,
        mut dst_cursor: MappingCursor,
    ) -> Result<(), TwzError> {
        log::info!(
            "setup_cow_range: src_cursor {:?}, dst_cursor {:?}, src_root {:x}, dst_root {:x}",
            src_cursor,
            dst_cursor,
            self.root_address().raw(),
            dest.root_address().raw()
        );
        let start_level = self.start_level;
        assert!(start_level == dest.start_level);
        let root = self.root_mut();
        root.setup_cow_range(
            dest.root_mut(),
            &mut src_cursor,
            &mut dst_cursor,
            start_level,
        )
        .ok_or(ResourceError::OutOfMemory)?;
        Ok(())
    }

    pub fn print_tables(&self) {
        log::info!(
            "=== PAGE TABLES FROM ROOT {:x} ===",
            self.root_address().raw()
        );
        self.root()
            .print_tables_recursive(self.start_level(), VirtAddr::new(0).unwrap(), 0);
    }

    pub fn cow_at(&mut self, cursor: MappingCursor) -> Option<()> {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.cow_copy(&mut consist, &cursor, level)
    }
}
