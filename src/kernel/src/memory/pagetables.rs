use core::ops::{Index, IndexMut};

use crate::arch::{
    address::{PhysAddr, VirtAddr},
    pagetables::{Entry, EntryFlags, PAGE_TABLE_ENTRIES},
};

use super::map::Mapping;

#[repr(transparent)]
pub struct Table {
    entries: [Entry; PAGE_TABLE_ENTRIES],
}

pub struct TableOpData {
    entries: [Entry; 5],
    level: usize,
}

impl TableOpData {
    fn new() -> Self {
        Self {
            entries: [Entry::new_unused(); 5],
            level: 4,
        }
    }

    fn level(&self) -> usize {
        self.level
    }

    fn final_entry(&self) -> &Entry {
        &self.entries[0]
    }

    fn non_final_entries(&self) -> &[Entry] {
        &self.entries[1..]
    }
}

impl Table {
    pub fn set_count(&mut self, count: usize) {
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

    pub fn zero(&mut self) {
        todo!()
    }

    pub fn map_leaf(&mut self, index: usize, mapping: &Mapping, off: usize) -> Option<Entry> {
        let old_count = self.read_count();
        let entry = &mut self[index];
        let ret = entry.clone();
        let flags = todo!();
        let addr = todo!();
        *entry = Entry::new(addr, flags);
        if ret.is_unused() {
            self.set_count(old_count + 1);
            None
        } else {
            Some(ret)
        }
    }

    pub fn read_leaf(&mut self, index: usize) -> Option<Entry> {
        let entry = &mut self[index];
        let ret = entry.clone();
        if ret.is_unused() {
            None
        } else {
            Some(ret)
        }
    }

    pub fn unmap_leaf(&mut self, index: usize) -> Option<Entry> {
        let entry = &mut self[index];
        let old_count = self.read_count();
        let ret = entry.clone();
        let flags = todo!();
        let addr = todo!();
        *entry = Entry::new_unused();
        if ret.is_unused() {
            None
        } else {
            assert!(old_count > 0);
            self.set_count(old_count - 1);
            Some(ret)
        }
    }

    fn destroy(&mut self) {
        todo!()
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

    pub fn recur_op<F>(
        &mut self,
        addr: VirtAddr,
        level: usize,
        pop: Option<EntryFlags>,
        f: F,
    ) -> Option<TableOpData>
    where
        F: Fn(&mut Table, usize) -> Option<Entry>,
    {
        let index = Self::get_index(addr, level);
        if let Some(flags) = pop {
            self.populate(index, flags);
        }
        let is_leaf = Self::is_leaf(addr, level);
        if is_leaf {
            let entry = f(self, index)?;
            Some(TableOpData {
                entries: [
                    entry,
                    Entry::new_unused(),
                    Entry::new_unused(),
                    Entry::new_unused(),
                    Entry::new_unused(),
                ],
                level,
            })
        } else {
            if let Some(next_table) = self.next_table_mut(index) {
                let res = next_table
                    .recur_op(addr, level - 1, None, f)
                    .map(|mut data| {
                        data.entries[level] = self[index].clone();
                        data
                    });
                if next_table.read_count() == 0 {
                    next_table.destroy();
                    let old_count = self.read_count();
                    self[index] = Entry::new_unused();
                    self.set_count(old_count - 1);
                }
                res
            } else {
                None
            }
        }
    }

    pub fn map(&mut self, mapping: &Mapping, off: usize) -> Option<TableOpData> {
        // TODO: what happens if we map a 2MB on top of an existing 4KB?
        let addr = mapping
            .vaddr_start()
            .offset(off.try_into().unwrap())
            .unwrap();
        self.recur_op(addr, 3, Some(mapping.non_leaf_flags()), |table, index| {
            table.map_leaf(index, mapping, off)
        })
    }

    pub fn unmap(&mut self, addr: VirtAddr) -> Option<TableOpData> {
        self.recur_op(addr, 3, None, |table, index| table.unmap_leaf(index))
    }

    //pub fn readmap(&mut self, addr: VirtAddr) -> Option<TableOpData> {
    //    self.recur_op(addr, 3, None, |table, index| table.read_leaf(index))
    //}

    fn populate(&mut self, index: usize, flags: EntryFlags) {
        let entry = &mut self[index];
        if entry.is_unused() {
            let count = self.read_count();
            *entry = Entry::new(todo!(), flags);
            self.set_count(count + 1);
        }
    }

    pub fn next_table_mut(&mut self, index: usize) -> Option<&mut Table> {
        let entry = self[index];
        if entry.is_unused() || entry.is_huge() {
            return None;
        }
        let addr = entry.addr().kernel_vaddr();
        unsafe { Some(&mut *(addr.as_mut_ptr::<Table>())) }
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

pub struct Mapper {
    root: PhysAddr,
}

impl Mapper {
    pub fn new(root: PhysAddr) -> Self {
        Self { root }
    }

    pub fn root_mut(&self) -> &mut Table {
        unsafe { &mut *(self.root.kernel_vaddr().as_mut_ptr::<Table>()) }
    }

    fn can_map_at(mapping: &Mapping, off: usize, level: usize) -> bool {
        let this_addr = mapping
            .vaddr_start()
            .offset(off.try_into().unwrap())
            .unwrap();
        let this_phys = mapping
            .paddr_start()
            .offset(off.try_into().unwrap())
            .unwrap();
        let remain = mapping.length() - off;
        let page_size = Table::level_to_page_size(level);
        this_addr.is_aligned_to(page_size)
            && remain >= page_size
            && this_phys.is_aligned_to(page_size)
    }

    pub fn map(&mut self, mapping: &Mapping) {
        let root = self.root_mut();
        let mut off = 0;
        while off < mapping.length() {
            let remain = mapping.length() - off;
            let this_level = if Self::can_map_at(mapping, off, 2) {
                2
            } else if Self::can_map_at(mapping, off, 1) {
                1
            } else {
                0
            };
            let this_len = Table::level_to_page_size(this_level);
            root.map(mapping, off);
            off += this_len;
        }
    }

    pub fn unmap(&mut self, addr: VirtAddr, len: usize) {
        let root = self.root_mut();
        let mut off = 0;
        while off < len {
            let entry = root.unmap(addr.offset(off.try_into().unwrap()).unwrap());
            off += if let Some(entry) = entry {
                Table::level_to_page_size(entry.level())
            } else {
                4096
            };
        }
    }
}
