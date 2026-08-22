use twizzler_rt_abi::error::TwzError;

use super::{MapInfo, MappingCursor, MappingSettings, PhysAddrProvider, consistency::Consistency};
use crate::{
    arch::{
        VirtAddr,
        address::PhysAddr,
        memory::pagetables::{Entry, EntryFlags, Table},
    },
    memory::tracker::FrameAllocator,
    obj::pagetables::{ObjectPageTable, mapprobe},
    thread::current_thread_ref,
};

/// Manager for a set of page tables. This is the primary interface for manipulating a set of page
/// tables.
pub struct Mapper {
    root: PhysAddr,
    start_level: usize,
    generation: u64,
}

impl Mapper {
    /// Construct a new set of tables from an existing root page.
    pub fn new(root: PhysAddr) -> Self {
        Self {
            root,
            start_level: Table::top_level(),
            generation: 0,
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

    pub fn generation(&self) -> u64 {
        self.generation
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
        self.generation += 1;
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

    /// How many frames a [`Self::map`] of `cursor` may allocate, given what is already installed.
    /// See [`Table::tables_needed`] -- conservative, and only valid under the page-table lock the
    /// matching `map` will be performed under.
    pub fn tables_needed(&self, cursor: &MappingCursor) -> usize {
        let t = mapprobe::start();
        let mut examined = 0;
        let need = self
            .root()
            .tables_needed(cursor, self.start_level, &mut examined);
        mapprobe::record(&mapprobe::TN_NS, t);
        mapprobe::tick(&mapprobe::TN_CALLS);
        mapprobe::add_if_on(&mapprobe::TN_ENTRIES, examined as u64);
        mapprobe::add_if_on(&mapprobe::TN_NEED, need as u64);
        // What geometry would have charged, so the saving is a measured difference rather than a
        // difference between two runs. Computed only with the probe on -- it is not free.
        if mapprobe::MAP_PROBE {
            mapprobe::add(
                &mapprobe::TN_MAX,
                cursor.max_number_new_tables(self.start_level, 0) as u64,
            );
        }
        need
    }

    /// How many frames a [`Self::cow_at`] of `cursor` may allocate. See
    /// [`Table::cow_tables_needed`] -- a different allocation shape from `map`'s, so it is a
    /// different predictor, and the same page-table-lock rule applies.
    pub fn cow_tables_needed(&self, cursor: &MappingCursor) -> usize {
        let t = mapprobe::start();
        let mut examined = 0;
        let need = self
            .root()
            .cow_tables_needed(cursor, self.start_level, &mut examined);
        mapprobe::record(&mapprobe::TN_NS, t);
        mapprobe::tick(&mapprobe::TN_CALLS);
        mapprobe::add_if_on(&mapprobe::TN_ENTRIES, examined as u64);
        mapprobe::add_if_on(&mapprobe::TN_NEED, need as u64);
        if mapprobe::MAP_PROBE {
            mapprobe::add(
                &mapprobe::TN_MAX,
                cursor.max_number_new_tables(self.start_level, 0) as u64,
            );
        }
        need
    }

    /// Map a set of physical pages into the tables with the provided settings.
    pub fn map(
        &mut self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        consist: &mut Consistency,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        let level = self.start_level;
        let root = self.root_mut();
        let r = root.map(consist, cursor, level, phys, fa);
        self.generation += 1;
        let t_flush = crate::obj::pagetables::mapprobe::start();
        consist.flush_cache();
        crate::obj::pagetables::mapprobe::record(
            &crate::obj::pagetables::mapprobe::W_FLUSH_NS,
            t_flush,
        );
        r
    }

    #[must_use]
    /// Unmap a region from the page tables. The deferred operations must be run, and must be run
    /// AFTER unlocking any page table locks. `released` reports an object page table whose
    /// reference this unmap dropped (see [Table::unmap]).
    pub fn unmap(
        &mut self,
        cursor: MappingCursor,
        consist: &mut Consistency,
        fa: &mut FrameAllocator,
        released: &mut Option<PhysAddr>,
    ) -> Result<bool, TwzError> {
        let level = self.start_level;
        log::trace!(
            "unmap: cursor {:?}, root {:x}, level {}",
            cursor,
            self.root_address().raw(),
            level
        );
        let root = self.root_mut();
        let r = root.unmap(consist, cursor, level, fa, released);
        log::trace!("unmap: done");
        self.generation += 1;
        consist.flush_cache();
        r
    }

    /// Change a region to use new mapping settings.
    pub fn change(
        &mut self,
        cursor: MappingCursor,
        settings: &MappingSettings,
        consist: &mut Consistency,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        let level = self.start_level;
        log::trace!(
            "change: cursor {:?}, root {:x}, level {}, settings {:?}",
            cursor,
            self.root_address().raw(),
            level,
            settings
        );
        let root = self.root_mut();
        let r = root.change(consist, cursor, level, settings, fa);
        self.generation += 1;
        log::trace!("change: done");
        consist.flush_cache();
        r
    }

    /// Read the map of a single address (the start of the cursor). If there is a mapping at the
    /// specified location, return the mapping information. Otherwise, return Err with a length
    /// that specifies how much the cursor may advance before calling this function again to
    /// check for a new mapping.
    pub(super) fn do_read_map(&self, cursor: &MappingCursor) -> Result<MapInfo, usize> {
        let level = self.start_level;
        if current_thread_ref().is_some() && cursor.start().raw() == 0x3ffff000 {
            log::trace!(
                "read_map: cursor {:?}, root {:x}, level {}",
                cursor,
                self.root_address().raw(),
                level
            );
        }
        let root = self.root();
        let x = root.readmap(cursor, level);
        if current_thread_ref().is_some() && cursor.start().raw() == 0x3ffff000 {
            log::trace!("read_map: done: {:?}", x);
        }
        x
    }

    pub fn is_empty_at_level(&self, cursor: &MappingCursor, level: usize) -> bool {
        let start_level = self.start_level;
        let root = self.root();
        root.is_empty_at_level(cursor, level, start_level)
    }

    pub fn set_start_level(&mut self, start_level: usize) {
        self.start_level = start_level;
    }

    pub fn start_level(&self) -> usize {
        self.start_level
    }

    /// Map an object's page tables in. Returns true if a new reference to the object table was
    /// taken (see [Table::object_map]).
    pub fn object_map(
        &mut self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
        consist: &mut Consistency,
        fa: &mut FrameAllocator,
    ) -> Result<bool, TwzError> {
        let level = self.start_level;
        let root = self.root_mut();
        let r = object_tables
            .with_mapper(|mapper| root.object_map(consist, cursor, level, mapper, fa, settings));
        self.generation += 1;
        consist.flush_cache();
        r
    }

    pub fn with_dirty_bits(
        &mut self,
        cursor: MappingCursor,
        mut f: impl FnMut(MapInfo) -> bool,
        consist: &mut Consistency,
    ) -> Result<bool, TwzError> {
        let level = self.start_level;
        log::trace!(
            "with_dirty_bits: cursor {:?}, root {:x}, level {}",
            cursor,
            self.root_address().raw(),
            level
        );
        let root = self.root_mut();
        let d = root.with_dirty_bits(cursor, level, consist, &mut f)?;
        if d {
            self.generation += 1;
        }
        consist.flush_cache();
        log::trace!("with_dirty_bits: done");
        Ok(d)
    }

    pub fn is_object_mapped(&self, cursor: MappingCursor, settings: MappingSettings) -> bool {
        let level = self.start_level;
        let root = self.root();
        log::trace!(
            "is_object_mapped: cursor {:?}, root {:x}, level {}",
            cursor,
            self.root_address().raw(),
            level
        );
        let x = root.is_object_mapped(cursor, level, settings);
        log::trace!("is_object_mapped: result {}", x);
        x
    }

    pub fn get_table_addr(
        &mut self,
        level: usize,
        fa: &mut FrameAllocator,
    ) -> Result<PhysAddr, TwzError> {
        log::trace!(
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
            table.populate(0, EntryFlags::intermediate(), fa)?;
            table_phys = table[0].table_addr();
            table = table.next_table_mut(0).unwrap();
        }

        Ok(table_phys)
    }

    /// The address [Self::get_table_addr] would return for `level`, without allocating anything.
    /// `None` if that table does not exist yet.
    pub fn peek_table_addr(&self, level: usize) -> Option<PhysAddr> {
        let mut table_phys = self.root_address();
        let mut table = self.root();
        for _ in 0..(self.start_level.checked_sub(level)?) {
            let next = table.next_table(0)?;
            table_phys = table[0].table_addr();
            table = next;
        }
        Some(table_phys)
    }

    pub fn split_to_level(
        &mut self,
        addr: VirtAddr,
        level: usize,
        consist: &mut Consistency,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        let start_level = self.start_level;
        let root = self.root_mut();
        let r = root.split_to_level(consist, addr, start_level, level, fa);
        self.generation += 1;
        consist.flush_cache();
        r
    }

    pub fn setup_cow_range(
        &mut self,
        dest: &mut Mapper,
        mut src_cursor: MappingCursor,
        mut dst_cursor: MappingCursor,
        consist: &mut Consistency,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        log::trace!(
            "setup_cow_range: src_cursor {:?}, dst_cursor {:?}, src_root {:x}, dst_root {:x}",
            src_cursor,
            dst_cursor,
            self.root_address().raw(),
            dest.root_address().raw()
        );
        let start_level = self.start_level;
        self.generation += 1;
        let root = self.root_mut();
        while src_cursor.remaining() > 0 && dst_cursor.remaining() > 0 {
            log::trace!(
                "top level setup_cow_range: src_cursor {:?}, dst_cursor {:?}, start_level {}",
                src_cursor,
                dst_cursor,
                start_level
            );
            root.setup_cow_range(
                dest.root_mut(),
                &mut src_cursor,
                &mut dst_cursor,
                start_level,
                consist,
                start_level - 1,
                fa,
            )?;
        }
        consist.flush_cache();
        Ok(())
    }

    pub fn setup_zero_range(
        &mut self,
        mut cursor: MappingCursor,
        consist: &mut Consistency,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        log::trace!(
            "setup_zero_range: cursor {:?}, root {:x}",
            cursor,
            self.root_address().raw(),
        );
        let start_level = self.start_level;
        self.generation += 1;
        let root = self.root_mut();
        while cursor.remaining() > 0 {
            log::trace!(
                "top level setup_zero_range: cursor {:?}, start_level {}",
                cursor,
                start_level
            );
            root.setup_zero_range(consist, &mut cursor, start_level, fa)?;
        }
        consist.flush_cache();
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

    pub fn cow_at(
        &mut self,
        cursor: MappingCursor,
        consist: &mut Consistency,
        mark_dirty: bool,
        fa: &mut FrameAllocator,
    ) -> Result<bool, TwzError> {
        let level = self.start_level;
        self.generation += 1;
        let root = self.root_mut();
        let r = root.cow_copy(consist, &cursor, level, mark_dirty, fa);
        consist.flush_cache();
        r
    }
}
