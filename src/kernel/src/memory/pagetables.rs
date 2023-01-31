use core::ops::{Index, IndexMut};

use crate::arch::{
    address::{PhysAddr, VirtAddr},
    context::{ArchCacheLineMgr, ArchTlbMgr},
    pagetables::{Entry, EntryFlags, PAGE_TABLE_ENTRIES},
};

use super::{
    context::MappingPerms,
    frame::PhysicalFrameFlags,
    map::{CacheType, Mapping},
};

#[repr(transparent)]
pub struct Table {
    entries: [Entry; PAGE_TABLE_ENTRIES],
}

impl Table {
    pub fn set_count(&mut self, count: usize) {
        // NOTE: this function doesn't need cache line or TLB flushing because the hardware never reads these bits.
        for b in 0..16 {
            if count & (1 << b) == 0 {
                self[b].set_avail_bit(false);
            } else {
                self[b].set_avail_bit(true);
            }
        }
    }

    pub fn read_count(&self) -> usize {
        let mut count = 0;
        for b in 0..16 {
            let bit = self[b].get_avail_bit();
            count |= if bit { 1 } else { 0 } << b;
        }
        count
    }

    fn is_leaf(addr: VirtAddr, level: usize) -> bool {
        level == 0 || addr.is_aligned_to(1 << (12 + 9 * level))
    }

    fn get_index(addr: VirtAddr, level: usize) -> usize {
        let shift = 12 + 9 * level;
        (u64::from(addr) >> shift) as usize & 0x1ff
    }

    fn level_to_page_size(level: usize) -> usize {
        if level > 3 {
            panic!("invalid level");
        }
        1 << (12 + 9 * level)
    }

    pub fn next_table_mut(&mut self, index: usize) -> Option<&mut Table> {
        let entry = self[index];
        if entry.is_unused() || entry.is_huge() {
            return None;
        }
        let addr = entry.addr().kernel_vaddr();
        unsafe { Some(&mut *(addr.as_mut_ptr::<Table>())) }
    }

    pub fn next_table(&self, index: usize) -> Option<&Table> {
        let entry = self[index];
        if entry.is_unused() || entry.is_huge() {
            return None;
        }
        let addr = entry.addr().kernel_vaddr();
        unsafe { Some(&*(addr.as_ptr::<Table>())) }
    }

    fn can_map_at(vaddr: VirtAddr, paddr: PhysFrame, remain: usize, level: usize) -> bool {
        logln!("==> {:?} {:?} {} {}", vaddr, paddr, remain, level);
        let page_size = Table::level_to_page_size(level);
        vaddr.is_aligned_to(page_size)
            && remain >= page_size
            && paddr.addr.is_aligned_to(page_size)
            && Self::can_map_at_level(level)
            && paddr.len >= page_size
    }

    fn populate(&mut self, index: usize, flags: EntryFlags) {
        let count = self.read_count();
        let entry = &mut self[index];
        if entry.is_unused() {
            let frame = crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED);
            *entry = Entry::new(frame.start_address().as_u64().try_into().unwrap(), flags);
            self.set_count(count + 1);
        }
    }

    fn update_entry(
        &mut self,
        consist: &mut Consistency,
        index: usize,
        new_entry: Entry,
        vaddr: VirtAddr,
        was_terminal: bool,
        level: usize,
    ) {
        let count = self.read_count();
        let entry = &mut self[index];
        // TODO: check if we are doing a no-op, and early return

        let was_present = entry.is_present();
        let was_global = entry.is_global();
        *entry = new_entry;
        let entry_addr = VirtAddr::from(entry);
        consist.cl.flush(entry_addr);

        if was_present {
            consist.tlb.enqueue(vaddr, was_global, was_terminal, level)
        }

        if was_present && !new_entry.is_present() {
            self.set_count(count - 1);
        } else if !was_present && new_entry.is_present() {
            self.set_count(count + 1);
        } else {
            // TODO: we may be able to remove this write if we know we're not modifying entries whose avail bits we use.
            self.set_count(count);
        }
    }

    fn map(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        mut phys: &mut impl PhysAddrProvider,
        perms: MappingPerms,
        cache: CacheType,
    ) {
        let start_index = Self::get_index(cursor.start, level);
        for idx in start_index..PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];

            if entry.is_present() && (entry.is_huge() || level == 0) {
                phys.consume(Self::level_to_page_size(level));
                continue;
            }

            let paddr = phys.peek();

            if Self::can_map_at(cursor.start, paddr, cursor.remaining(), level) {
                panic!("TODO: flags (global, etc), and present");
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(paddr.addr, EntryFlags::new(perms, cache)),
                    cursor.start,
                    true,
                    level,
                );
                phys.consume(Self::level_to_page_size(level));
            } else {
                assert_ne!(level, 0);
                self.populate(idx, EntryFlags::intermediate());
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.map(consist, cursor, level - 1, phys, perms, cache);
            }

            if let Some(next) = cursor.advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
    }

    // TODO: freeing
    fn unmap(&mut self, consist: &mut Consistency, mut cursor: MappingCursor, level: usize) {
        let start_index = Self::get_index(cursor.start, level);
        for idx in start_index..PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];

            if entry.is_present() && (entry.is_huge() || level == 0) {
                self.update_entry(consist, idx, Entry::new_unused(), cursor.start, true, level);
            } else if entry.is_present() && level != 0 {
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.unmap(consist, cursor, level - 1);
            }

            if let Some(next) = cursor.advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
    }

    fn readmap(&self, cursor: &MappingCursor, level: usize) -> Option<MapInfo> {
        let index = Self::get_index(cursor.start, level);
        let entry = &self[index];
        if entry.is_present() && (entry.is_huge() || level == 0) {
            Some(MapInfo {
                vaddr: cursor.start,
                paddr: entry.addr(),
                perms: entry.flags().perms(),
                cache: entry.flags().cache_type(),
                psize: Self::level_to_page_size(level),
            })
        } else if entry.is_present() && level != 0 {
            let next_table = self.next_table(index).unwrap();
            next_table.readmap(cursor, level - 1)
        } else {
            None
        }
    }
}

impl Index<usize> for Table {
    type Output = Entry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

impl IndexMut<usize> for Table {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.entries[index]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PhysFrame {
    addr: PhysAddr,
    len: usize,
}

pub trait PhysAddrProvider {
    fn peek(&mut self) -> PhysFrame;
    fn consume(&mut self, len: usize);
}

#[derive(Debug, Clone, Copy)]
pub struct MappingCursor {
    start: VirtAddr,
    len: usize,
}

impl MappingCursor {
    pub fn new(start: VirtAddr, len: usize) -> Self {
        Self { start, len }
    }

    fn advance(mut self, len: usize) -> Option<Self> {
        if self.len <= len {
            return None;
        }
        let vaddr = self.start.offset(len as isize).ok()?;
        self.start = vaddr;
        self.len -= len;
        Some(self)
    }

    fn remaining(&self) -> usize {
        self.len
    }
}

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
        perms: MappingPerms,
        cache: CacheType,
    ) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.map(&mut consist, cursor, level, phys, perms, cache);
    }

    pub fn unmap(&mut self, cursor: MappingCursor) {
        let mut consist = Consistency::new(self.root);
        let level = self.start_level;
        let root = self.root_mut();
        root.unmap(&mut consist, cursor, level);
    }

    pub fn readmap(&self, cursor: MappingCursor) -> MapReader<'_> {
        MapReader {
            mapper: self,
            cursor,
        }
    }

    fn do_read_map(&self, cursor: &MappingCursor) -> Option<MapInfo> {
        let level = self.start_level;
        let root = self.root();
        root.readmap(cursor, level)
    }
}

pub struct MapReader<'a> {
    mapper: &'a Mapper,
    cursor: MappingCursor,
}

impl<'a> Iterator for MapReader<'a> {
    type Item = MapInfo;

    fn next(&mut self) -> Option<Self::Item> {
        let info = self.mapper.do_read_map(&self.cursor);
        if let Some(info) = info {
            self.cursor = self.cursor.advance(info.psize)?;
            Some(info)
        } else {
            self.cursor = self.cursor.advance(Table::level_to_page_size(0))?;
            None
        }
    }
}

#[derive(Debug, PartialEq, PartialOrd)]
pub struct MapInfo {
    vaddr: VirtAddr,
    paddr: PhysAddr,
    perms: MappingPerms,
    cache: CacheType,
    psize: usize,
}

impl MapInfo {
    pub fn new(
        vaddr: VirtAddr,
        paddr: PhysAddr,
        perms: MappingPerms,
        cache: CacheType,
        psize: usize,
    ) -> Self {
        Self {
            vaddr,
            paddr,
            perms,
            cache,
            psize,
        }
    }

    pub fn vaddr(&self) -> VirtAddr {
        self.vaddr
    }

    pub fn perms(&self) -> MappingPerms {
        self.perms
    }

    pub fn cache(&self) -> CacheType {
        self.cache
    }

    pub fn psize(&self) -> usize {
        self.psize
    }
}

struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
}

impl Consistency {
    fn new(target: PhysAddr) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
        }
    }
}

impl Drop for Consistency {
    fn drop(&mut self) {
        self.tlb.finish();
    }
}

#[cfg(test)]
mod test {
    use crate::memory::frame::PhysicalFrameFlags;

    use super::*;
    struct SimpleP {
        next: Option<PhysFrame>,
    }

    impl PhysAddrProvider for SimpleP {
        fn peek(&mut self) -> PhysFrame {
            if let Some(ref next) = self.next {
                return next.clone();
            } else {
                let f = crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED);
                self.next = Some(PhysFrame {
                    addr: f.start_address().as_u64().try_into().unwrap(),
                    len: f.size(),
                });
                self.peek()
            }
        }

        fn consume(&mut self, _len: usize) {
            self.next = None;
        }
    }
    #[test_case]
    fn test_mapper() {
        logln!("testing mapping");
        let mut m = Mapper::new(
            crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED)
                .start_address()
                .as_u64()
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            m.readmap(MappingCursor::new(VirtAddr::new(0).unwrap(), 0))
                .next(),
            None
        );
        assert_eq!(
            m.readmap(MappingCursor::new(VirtAddr::new(0).unwrap(), usize::MAX))
                .next(),
            None
        );

        // TODO: magic numbers
        let cur = MappingCursor::new(VirtAddr::new(0).unwrap(), 0x1000);
        let mut phys = SimpleP { next: None };
        m.map(cur, &mut phys, MappingPerms::READ, CacheType::WriteBack);

        let mut reader = m.readmap(cur);
        let read = reader.next().unwrap();
        assert_eq!(read.vaddr(), VirtAddr::new(0).unwrap());
        assert_eq!(read.psize(), 0x1000);
        assert_eq!(read.cache(), CacheType::WriteBack);
        assert_eq!(read.perms(), MappingPerms::READ);

        assert_eq!(reader.next(), None);
    }
}
