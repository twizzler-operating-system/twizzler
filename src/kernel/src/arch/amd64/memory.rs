use core::sync::atomic::{AtomicU8, Ordering};

use twizzler_abi::device::CacheType;
use x86_64::{structures::paging::PageTable, PhysAddr, VirtAddr};

fn translate_addr_inner(addr: VirtAddr, phys_mem_offset: VirtAddr) -> Option<PhysAddr> {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::page_table::FrameError;

    let (l4_table_frame, _) = Cr3::read();

    let table_indicies = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    let mut frame = l4_table_frame;

    for &index in &table_indicies {
        let virt = phys_mem_offset + frame.start_address().as_u64();
        let table_ptr: *const PageTable = virt.as_ptr();
        let table = unsafe { &*table_ptr };

        let entry = &table[index];
        frame = match entry.frame() {
            Ok(frame) => frame,
            Err(FrameError::FrameNotPresent) => return None,
            Err(FrameError::HugeFrame) => panic!("huge page not supported"),
        };
    }

    Some(frame.start_address() + u64::from(addr.page_offset()))
}

pub fn translate_addr(addr: VirtAddr, phys_mem_offset: VirtAddr) -> Option<PhysAddr> {
    translate_addr_inner(addr, phys_mem_offset)
}

use x86_64::structures::paging::OffsetPageTable;

use crate::memory::context::MapFlags;
use crate::memory::frame::{alloc_frame, PhysicalFrameFlags};
use crate::memory::{MapFailed, MappingInfo};

#[allow(clippy::missing_safety_doc)]
pub unsafe fn init(phys_mem_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(phys_mem_offset);
    OffsetPageTable::new(level_4_table, phys_mem_offset)
}

unsafe fn active_level_4_table(phys_mem_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;
    let (level_4_table_frame, _) = Cr3::read();
    let phys = level_4_table_frame.start_address();
    let virt = phys_mem_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    &mut *page_table_ptr
}

const PHYS_MEM_OFFSET: u64 = 0xffff800000000000;
/* TODO: hide this */
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    VirtAddr::new(pa.as_u64() + PHYS_MEM_OFFSET)
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Table {
    frame: PhysAddr,
}

impl Table {
    #[optimize(speed)]
    fn as_slice_mut(&mut self) -> &mut [u64] {
        let va = phys_to_virt(self.frame);
        unsafe { core::slice::from_raw_parts_mut(va.as_mut_ptr(), 512) }
    }

    #[optimize(speed)]
    fn as_slice(&self) -> &[u64] {
        let va = phys_to_virt(self.frame);
        unsafe { core::slice::from_raw_parts(va.as_ptr(), 512) }
    }

    #[optimize(speed)]
    fn map(&mut self, idx: usize, entry: u64) {
        let _existing = self.as_slice()[idx];
        //assert!(existing == 0 || existing == entry);
        self.as_slice_mut()[idx] = entry;
    }

    fn clear_entry(&mut self, idx: usize) {
        self.as_slice_mut()[idx] = 0;
    }

    #[optimize(speed)]
    fn is_entry(&self, idx: usize, is_last: bool) -> bool {
        let e = self.as_slice()[idx];
        e != 0 && ((e & 0b10000000) != 0 || is_last)
    }

    #[optimize(speed)]
    fn get_entry(&self, idx: usize, is_last: bool) -> Option<(PhysAddr, MapFlags)> {
        if !self.is_entry(idx, is_last) {
            return None;
        }
        let e = self.as_slice()[idx];
        let paddr = e & 0x7ffffffffffff000;
        let flags = e & !0x7ffffffffffff000;
        Some((PhysAddr::new(paddr), MapFlags::from_entry(flags)))
    }

    #[optimize(speed)]
    fn get_child_noalloc(&self, idx: usize) -> Option<Table> {
        let e = self.as_slice()[idx];
        if e & 0b10000000 != 0 {
            return None;
        }
        //assert!(e & 0b10000000 == 0);
        if e == 0 {
            None
        } else {
            let paddr = e & 0x7ffffffffffff000;
            Some(PhysAddr::new(paddr).into())
        }
    }

    #[optimize(speed)]
    fn get_child(&mut self, idx: usize, flags: u64) -> Option<Table> {
        if self.as_slice_mut()[idx] == 0 {
            let frame = alloc_frame(PhysicalFrameFlags::ZEROED);
            self.as_slice_mut()[idx] = frame.start_address().as_u64() | flags;
        }
        let e = self.as_slice_mut()[idx];
        assert!(e & 0b10000000 == 0);
        let paddr = e & 0x7ffffffffffff000;
        Some(PhysAddr::new(paddr).into())
    }
}

impl From<PhysAddr> for Table {
    fn from(frame: PhysAddr) -> Self {
        Self { frame }
    }
}
pub struct ArchMemoryContext {
    table_root: Table,
}

impl MapFlags {
    #[optimize(speed)]
    fn entry_bits(&self) -> u64 {
        let mut flags = 1;
        if self.contains(Self::WRITE) {
            flags |= 0b10;
        }
        if self.contains(Self::USER) {
            flags |= 0b100;
        }
        if self.contains(Self::GLOBAL) {
            flags |= 0b100000000;
        }
        if !self.contains(Self::EXECUTE) {
            flags |= 1u64 << 63;
        }
        flags
    }

    #[optimize(speed)]
    fn table_bits(&self) -> u64 {
        let mut flags = 3;
        if self.contains(Self::USER) {
            flags |= 0b100;
        }
        flags
    }

    #[optimize(speed)]
    fn from_entry(e: u64) -> Self {
        let mut flags = Self::READ;
        if e & 0b10 != 0 {
            flags.insert(Self::WRITE)
        }
        if e & 0b100 != 0 {
            flags.insert(Self::USER)
        }
        if e & 0b100000000 != 0 {
            flags.insert(Self::GLOBAL)
        }
        if e & (1u64 << 63) == 0 {
            flags.insert(Self::EXECUTE)
        }
        flags
    }
}

pub struct ArchMemoryContextSwitchInfo {
    target: u64,
}

// TODO: build a generic framework for "cached-feature-flag" (and use this for below and the one in amd64/thread.rs)
fn has_huge_pages() -> bool {
    static VAL: AtomicU8 = AtomicU8::new(0);
    let val = VAL.load(Ordering::SeqCst);
    match val {
        0 => {
            let x = x86::cpuid::CpuId::new()
                .get_extended_processor_and_feature_identifiers()
                .map(|f| f.has_1gib_pages())
                .unwrap_or_default();
            VAL.store(if x { 2 } else { 1 }, Ordering::SeqCst);
            x
        }
        1 => false,
        _ => true,
    }
}

const PAGE_SIZE_HUGE: usize = 1024 * 1024 * 1024;
const PAGE_SIZE_LARGE: usize = 2 * 1024 * 1024;
const PAGE_SIZE: usize = 0x1000;
impl ArchMemoryContext {
    pub fn new_blank() -> Self {
        let frame = alloc_frame(PhysicalFrameFlags::ZEROED);
        let mut table_root: Table = frame.start_address().into();
        for i in 256..512 {
            table_root.get_child(
                i,
                (MapFlags::EXECUTE
                    | MapFlags::WRITE
                    | MapFlags::READ
                    | MapFlags::WIRED
                    | MapFlags::GLOBAL)
                    .table_bits(),
            );
        }
        Self { table_root }
    }

    pub fn root(&self) -> PhysAddr {
        self.table_root.frame
    }

    pub fn get_switch_info(&self) -> ArchMemoryContextSwitchInfo {
        ArchMemoryContextSwitchInfo {
            target: self.root().as_u64(),
        }
    }

    pub fn clone_empty_user(&self) -> Self {
        let mut new = Self::new_blank();
        let table = new.table_root.as_slice_mut();
        table[256..512].clone_from_slice(&self.table_root.as_slice()[256..512]);
        new
    }

    pub fn from_existing_tables(table_root: PhysAddr) -> Self {
        Self {
            table_root: table_root.into(),
        }
    }

    pub fn current_tables() -> Self {
        unsafe { Self::from_existing_tables(PhysAddr::new(x86::controlregs::cr3())) }
    }

    pub fn get_map(&self, addr: VirtAddr) -> Option<MappingInfo> {
        let indexes = [
            addr.p4_index(),
            addr.p3_index(),
            addr.p2_index(),
            addr.p1_index(),
        ];
        let mut table = self.table_root;
        for (i, idx) in indexes.iter().enumerate() {
            let info = table.get_entry((*idx).into(), i == 3);
            let len = match i {
                1 => PAGE_SIZE_HUGE,
                2 => PAGE_SIZE_LARGE,
                3 => PAGE_SIZE,
                _ => 0,
            };
            if info.is_none() && i == 3 {
                return None;
            }
            if let Some((phys, flags)) = info {
                assert!(len > 0);
                return Some(MappingInfo::new(addr, phys, len, flags));
            }
            if let Some(next_table) = table.get_child_noalloc((*idx).into()) {
                table = next_table;
            } else {
                return None;
            }
        }
        None
    }

    #[optimize(speed)]
    pub fn premap(
        &mut self,
        start: VirtAddr,
        length: usize,
        page_size: usize,
        flags: MapFlags,
    ) -> Result<(), MapFailed> {
        let end = start + length;
        let mut count = 0usize;
        if page_size == PAGE_SIZE_HUGE && !has_huge_pages() {
            panic!("tried to map huge pages on a CPU that doesn't support them");
        }
        loop {
            let addr = start + count;
            let indexes = [
                addr.p4_index(),
                addr.p3_index(),
                addr.p2_index(),
                addr.p1_index(),
            ];
            if addr >= end {
                break;
            }
            let nr_recur = match page_size {
                PAGE_SIZE => 3,
                PAGE_SIZE_LARGE => 2,
                PAGE_SIZE_HUGE => 1,
                _ => unimplemented!(),
            };
            let mut table = self.table_root;
            for idx in indexes.iter().take(nr_recur) {
                table = table
                    .get_child((*idx).into(), flags.table_bits())
                    .ok_or(MapFailed::FrameAllocation)?
            }
            count += match nr_recur {
                1 => PAGE_SIZE_HUGE,
                2 => PAGE_SIZE_LARGE,
                3 => PAGE_SIZE,
                _ => unreachable!(),
            };
        }
        Ok(())
    }

    pub fn unmap(&mut self, start: VirtAddr, length: usize) {
        /* TODO: Free frames? */
        let end = start + length;
        let mut count = 0usize;
        loop {
            let addr = start + count;
            let indexes = [
                addr.p4_index(),
                addr.p3_index(),
                addr.p2_index(),
                addr.p1_index(),
            ];

            if addr > end {
                break;
            }

            let mut table = self.table_root;
            let mut level = 3;
            for (i, idx) in indexes.iter().enumerate() {
                if table.is_entry((*idx).into(), i == 3) {
                    table.clear_entry((*idx).into());
                    break;
                }
                let next = table.get_child_noalloc((*idx).into());
                if let Some(next) = next {
                    table = next;
                    level -= 1;
                } else {
                    break;
                }
            }
            let thiscount = match level {
                0 => PAGE_SIZE,
                1 => PAGE_SIZE_LARGE,
                2 => PAGE_SIZE_HUGE,
                3 => PAGE_SIZE_HUGE * 512,
                _ => unreachable!(),
            };
            count += thiscount;
        }
    }

    pub fn map(
        &mut self,
        start: VirtAddr,
        phys: PhysAddr,
        mut length: usize,
        flags: MapFlags,
        cache_type: CacheType,
    ) -> Result<(), MapFailed> {
        if start.as_u64().checked_add(length as u64).is_none() {
            length -= PAGE_SIZE;
        }
        let end = start + length;
        let mut count = 0usize;
        let cache_bits = match cache_type {
            CacheType::WriteBack => 0,
            CacheType::WriteCombining => 0, // TODO: is this correct?
            CacheType::WriteThrough => 1 << 3,
            CacheType::Uncachable => 1 << 4,
        };
        loop {
            let addr = start.as_u64().checked_add(count as u64);
            let addr = if let Some(addr) = addr {
                VirtAddr::new(addr)
            } else {
                return Ok(());
            };
            let frame = phys + count;
            let indexes = [
                addr.p4_index(),
                addr.p3_index(),
                addr.p2_index(),
                addr.p1_index(),
            ];
            if addr >= end {
                break;
            }
            let nr_recur = if addr.is_aligned(PAGE_SIZE_HUGE as u64)
                && end.is_aligned(PAGE_SIZE_HUGE as u64)
                && frame.is_aligned(PAGE_SIZE_HUGE as u64)
                && has_huge_pages()
            {
                1
            } else if addr.is_aligned(PAGE_SIZE_LARGE as u64)
                && end.is_aligned(PAGE_SIZE_LARGE as u64)
                && frame.is_aligned(PAGE_SIZE_LARGE as u64)
            {
                2
            } else {
                3
            };
            //logln!("mapping {:?} {:?} {}", addr, frame, nr_recur);

            let mut table = self.table_root;
            for idx in indexes.iter().take(nr_recur) {
                table = table
                    .get_child((*idx).into(), flags.table_bits())
                    .ok_or(MapFailed::FrameAllocation)?
            }
            table.map(
                indexes[nr_recur].into(),
                frame.as_u64()
                    | flags.entry_bits()
                    | if nr_recur < 3 { 0b10000000 } else { 0 }
                    | cache_bits,
            );
            count += match nr_recur {
                1 => PAGE_SIZE_HUGE,
                2 => PAGE_SIZE_LARGE,
                3 => PAGE_SIZE,
                _ => unreachable!(),
            };
        }
        Ok(())
    }
}

impl ArchMemoryContextSwitchInfo {
    /// Switch context.
    /// # Safety
    /// The context must be valid.
    pub unsafe fn switch(&self) {
        x86::controlregs::cr3_write(self.target)
    }
}
