use alloc::sync::Arc;
use core::{
    fmt::Debug,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use twizzler_abi::{
    device::{CacheType, MMIO_OFFSET},
    meta::MetaInfo,
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
};

use super::{
    range::{PageRangeTree, PageStatus},
    Object, ObjectRef, PageNumber,
};
use crate::{
    arch::memory::phys_to_virt,
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS},
        pagetables::{MappingFlags, MappingSettings},
        tracker::{alloc_frame, free_frame, FrameAllocFlags, FrameAllocator},
        PhysAddr, VirtAddr,
    },
    mutex::LockGuard,
    obj::range::GetPageFlags,
};

/// An object page can be either a physical frame (allocatable memory) or a static physical address
/// (wired). This will likely be overhauled soon.
#[derive(Debug)]
enum FrameOrWired {
    Frame(FrameRef),
    Wired(PhysAddr, usize),
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    struct PageSyncFlags: u32 {
        const LOCKED = 1 << 0;
    }
}

#[derive(Debug)]
struct PageSync {
    flags: PageSyncFlags,
}

pub struct Page {
    frame: FrameOrWired,
    map_settings: MappingSettings,
}

impl Debug for Page {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Page")
            .field("frame", &self.frame)
            .field("map_settings", &self.map_settings)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct PageRef {
    page: Arc<Page>,
    pn: usize,
    count: usize,
}

impl Drop for Page {
    fn drop(&mut self) {
        match self.frame {
            FrameOrWired::Frame(f) => {
                free_frame(f);
            }
            // TODO: this could be a wired, but freeable page (see kernel quick control objects).
            FrameOrWired::Wired(_, _) => {}
        }
    }
}

impl Page {
    pub fn new(frame: FrameRef) -> Self {
        Self {
            frame: FrameOrWired::Frame(frame),
            map_settings: MappingSettings::new(
                Protections::all(),
                CacheType::WriteBack,
                MappingFlags::USER,
            ),
        }
    }

    pub fn new_wired(pa: PhysAddr, size: usize, cache_type: CacheType) -> Self {
        Self {
            frame: FrameOrWired::Wired(pa, size),
            map_settings: MappingSettings::new(Protections::all(), cache_type, MappingFlags::USER),
        }
    }

    pub fn nr_pages(&self) -> usize {
        (match self.frame {
            FrameOrWired::Frame(frame) => frame.size(),
            FrameOrWired::Wired(_, s) => s,
        }) / PageNumber::PAGE_SIZE
    }

    pub fn as_virtaddr(&self) -> VirtAddr {
        phys_to_virt(self.physical_address())
    }

    pub fn as_slice(&self, pnum: usize) -> &[u8] {
        let len = match self.frame {
            FrameOrWired::Frame(f) => f.size(),
            FrameOrWired::Wired(_, s) => s,
        } - pnum * PageNumber::PAGE_SIZE;
        unsafe {
            core::slice::from_raw_parts(
                self.as_virtaddr()
                    .offset(pnum * PageNumber::PAGE_SIZE)
                    .unwrap()
                    .as_ptr(),
                len,
            )
        }
    }

    pub unsafe fn get_mut_to_val<T>(&self, offset: usize) -> *mut T {
        /* TODO: enforce alignment and size of offset */
        /* TODO: once we start optimizing frame zeroing, we need to make the frame as non-zeroed
         * here */
        let va = self.as_virtaddr();
        let bytes = va.as_mut_ptr::<u8>();
        bytes.add(offset) as *mut T
    }

    pub fn as_mut_slice(&self, pnum: usize) -> &mut [u8] {
        let len = match self.frame {
            FrameOrWired::Frame(f) => f.size(),
            FrameOrWired::Wired(_, s) => s,
        } - pnum * PageNumber::PAGE_SIZE;
        unsafe {
            core::slice::from_raw_parts_mut(
                self.as_virtaddr()
                    .offset(pnum * PageNumber::PAGE_SIZE)
                    .unwrap()
                    .as_mut_ptr(),
                len,
            )
        }
    }

    pub fn physical_address(&self) -> PhysAddr {
        match self.frame {
            FrameOrWired::Frame(f) => f.start_address(),
            FrameOrWired::Wired(p, _) => p,
        }
    }

    pub fn copy_from(&self, other: &Page, doff: usize, soff: usize, len: usize) {
        match self.frame {
            FrameOrWired::Frame(frame) => match other.frame {
                FrameOrWired::Frame(otherframe) => {
                    frame.copy_contents_from(otherframe, doff, soff, len)
                }
                FrameOrWired::Wired(phys_addr, _) => {
                    frame.copy_contents_from_physaddr(doff, phys_addr.offset(doff).unwrap(), len)
                }
            },
            FrameOrWired::Wired(_phys_addr, _) => todo!(),
        }
    }

    pub fn map_settings(&self) -> MappingSettings {
        self.map_settings
    }
}

impl PageRef {
    pub fn new(page: Arc<Page>, pn: usize, count: usize) -> Self {
        Self { page, pn, count }
    }

    pub fn adjust_down(&self, off: usize) -> Self {
        assert!(off <= self.pn);
        Self {
            page: self.page.clone(),
            pn: self.pn - off,
            count: self.count + off,
        }
    }

    pub fn adjust(&self, off: usize) -> Self {
        assert!(off < self.count);
        Self {
            page: self.page.clone(),
            pn: self.pn + off,
            count: self.count - off,
        }
    }

    pub fn trimmed(&self, count: usize) -> Self {
        assert!(count <= self.count);
        Self {
            page: self.page.clone(),
            pn: self.pn,
            count,
        }
    }

    pub fn nr_pages(&self) -> usize {
        self.count
    }

    pub fn page_offset(&self) -> usize {
        self.pn
    }

    pub fn ref_count(&self) -> usize {
        Arc::strong_count(&self.page)
    }

    pub fn as_virtaddr(&self) -> VirtAddr {
        self.page
            .as_virtaddr()
            .offset(self.pn * PageNumber::PAGE_SIZE)
            .unwrap()
    }

    pub fn physical_address(&self) -> PhysAddr {
        self.page
            .physical_address()
            .offset(self.pn * PageNumber::PAGE_SIZE)
            .unwrap()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.page.as_slice(self.pn)[0..(self.count * PageNumber::PAGE_SIZE)]
    }

    pub fn as_mut_slice(&self) -> &mut [u8] {
        &mut self.page.as_mut_slice(self.pn)[0..(self.count * PageNumber::PAGE_SIZE)]
    }

    pub unsafe fn get_mut_to_val<T>(&self, offset: usize) -> *mut T {
        self.page
            .get_mut_to_val(offset + self.pn * PageNumber::PAGE_SIZE)
    }

    pub fn copy_from(&mut self, other: &Self) {
        let len = self.count.min(other.count);
        self.page.copy_from(
            &other.page,
            self.pn * PageNumber::PAGE_SIZE,
            other.pn * PageNumber::PAGE_SIZE,
            len * PageNumber::PAGE_SIZE,
        )
    }

    pub fn map_settings(&self) -> MappingSettings {
        self.page.map_settings
    }
}

impl Object {
    /// Try to write a value to an object at a given offset and signal a wakeup.
    ///
    /// If the object does not have a page at the given offset, the write will not be performed, but
    /// a wakeup will still occur.
    pub unsafe fn try_write_val_and_signal<T>(&self, offset: usize, val: T, wakeup_count: usize) {
        assert!(!self.use_pager());
        {
            let mut obj_page_tree = self.lock_page_tree();
            let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
            let page_offset = offset % PageNumber::PAGE_SIZE;

            if let PageStatus::Ready(page, _) =
                obj_page_tree.get_page(page_number, GetPageFlags::WRITE, None)
            {
                let t = page.get_mut_to_val::<T>(page_offset);
                *t = val;
            }
        }
        self.wakeup_word(offset, wakeup_count);
        crate::syscall::sync::requeue_all();
    }

    pub unsafe fn read_atomic_u64(&self, offset: usize) -> u64 {
        assert!(!self.use_pager());
        let mut obj_page_tree = self.lock_page_tree();
        let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
        let page_offset = offset % PageNumber::PAGE_SIZE;

        if let PageStatus::Ready(page, _) =
            obj_page_tree.get_page(page_number, GetPageFlags::empty(), None)
        {
            let t = page.get_mut_to_val::<AtomicU64>(page_offset);
            (*t).load(Ordering::SeqCst)
        } else {
            0
        }
    }

    #[track_caller]
    pub fn ensure_in_core<'a>(
        self: &'a Arc<Object>,
        mut page_tree: LockGuard<'a, PageRangeTree>,
        page_number: PageNumber,
    ) -> LockGuard<'a, PageRangeTree> {
        if matches!(
            page_tree.try_get_page(page_number, GetPageFlags::empty()),
            PageStatus::NoPage
        ) {
            if self.use_pager() {
                drop(page_tree);
                crate::pager::get_object_page(self, page_number);
                page_tree = self.lock_page_tree();
            } else {
                let flags = FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK;
                let pages_per_large = PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size();
                let large_page_number = page_number.align_down(pages_per_large);

                let mut entries =
                    page_tree.range(large_page_number..(large_page_number.offset(pages_per_large)));
                let all_empty = entries.all(|e| e.1.is_empty());

                //log::info!("{} ==> {} {}", self.id(), all_empty, page_number);

                if !large_page_number.is_zero()
                    && page_number
                        < PageNumber::from_offset(MAX_SIZE - PHYS_LEVEL_LAYOUTS[1].size())
                    && all_empty
                {
                    let mut frame_allocator = FrameAllocator::new(flags, PHYS_LEVEL_LAYOUTS[1]);
                    if let Some(frame) = frame_allocator.try_allocate() {
                        let page = Arc::new(Page::new(frame));
                        assert_eq!(frame.size(), PHYS_LEVEL_LAYOUTS[1].size());
                        let page = PageRef::new(page, 0, pages_per_large);
                        let mut frame_allocator = FrameAllocator::new(flags, PHYS_LEVEL_LAYOUTS[0]);
                        log::trace!(
                            "{}: mapping {} for {}",
                            self.id(),
                            large_page_number,
                            page_number
                        );
                        if page_tree
                            .add_page(large_page_number, page, Some(&mut frame_allocator))
                            .is_none()
                        {
                            log::warn!("failed to map large page {}", large_page_number);
                        }
                    }
                    return page_tree;
                }

                let mut frame_allocator = FrameAllocator::new(flags, PHYS_LEVEL_LAYOUTS[0]);
                if let Some(frame) = frame_allocator.try_allocate() {
                    let page = Arc::new(Page::new(frame));
                    let page = PageRef::new(page, 0, 1);
                    page_tree.add_page(page_number, page, Some(&mut frame_allocator));
                }
            }
        }
        page_tree
    }

    pub fn read_meta(self: &ObjectRef, can_wait: bool) -> Option<MetaInfo> {
        let mut obj_page_tree = self.lock_page_tree();
        let page_number = PageNumber::from_offset(MAX_SIZE - NULLPAGE_SIZE);

        if let PageStatus::Ready(page, _) =
            obj_page_tree.get_page(page_number, GetPageFlags::empty(), None)
        {
            unsafe {
                let t = page.get_mut_to_val::<MetaInfo>(0);
                Some(t.read())
            }
        } else {
            if !can_wait {
                return None;
            }
            obj_page_tree = self.ensure_in_core(obj_page_tree, page_number);
            drop(obj_page_tree);
            self.read_meta(can_wait)
        }
    }

    pub fn write_meta(&self, meta: MetaInfo, can_wait: bool) -> bool {
        assert!(!self.use_pager());
        let mut obj_page_tree = self.lock_page_tree();
        let page_number = PageNumber::from_offset(MAX_SIZE - NULLPAGE_SIZE);

        if let PageStatus::Ready(page, _) =
            obj_page_tree.get_page(page_number, GetPageFlags::WRITE, None)
        {
            unsafe {
                let t = page.get_mut_to_val::<MetaInfo>(0);
                t.write(meta);
            }
            true
        } else {
            let mut flags = FrameAllocFlags::ZEROED;
            if can_wait {
                flags.insert(FrameAllocFlags::WAIT_OK);
            }
            let mut frame_allocator = FrameAllocator::new(flags, PHYS_LEVEL_LAYOUTS[0]);

            if let Some(frame) = frame_allocator.try_allocate() {
                let page = Arc::new(Page::new(frame));
                let page = PageRef::new(page, 0, 1);
                unsafe {
                    let t = page.get_mut_to_val::<MetaInfo>(0);
                    t.write(meta);
                }
                if obj_page_tree
                    .add_page(page_number, page, Some(&mut frame_allocator))
                    .is_some()
                {
                    return true;
                }
            }
            if !can_wait {
                return false;
            }
            self.write_meta(meta, can_wait)
        }
    }

    pub unsafe fn read_atomic_u32(&self, offset: usize) -> u32 {
        assert!(!self.use_pager());
        let mut obj_page_tree = self.lock_page_tree();
        let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
        let page_offset = offset % PageNumber::PAGE_SIZE;

        if let PageStatus::Ready(page, _) =
            obj_page_tree.get_page(page_number, GetPageFlags::empty(), None)
        {
            let t = page.get_mut_to_val::<AtomicU32>(page_offset);
            (*t).load(Ordering::SeqCst)
        } else {
            0
        }
    }

    pub fn write_base<T>(&self, info: &T) {
        self.write_at(info, NULLPAGE_SIZE);
    }

    pub fn write_at<T>(&self, info: &T, offset: usize) {
        let bytes = info as *const T as *const u8;
        let len = core::mem::size_of::<T>();
        self.write_bytes(bytes, len, offset);
    }

    pub fn write_bytes(&self, bytes: *const u8, len: usize, mut offset: usize) {
        unsafe {
            let mut obj_page_tree = self.lock_page_tree();
            let bytes = core::slice::from_raw_parts(bytes, len);
            let mut count = 0;
            while count < len {
                let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
                let page_offset = offset % NULLPAGE_SIZE;
                let thislen = core::cmp::min(NULLPAGE_SIZE - page_offset, len - count);

                if let PageStatus::Ready(page, _) =
                    obj_page_tree.get_page(page_number, GetPageFlags::WRITE, None)
                {
                    let dest = &mut page.as_mut_slice()[page_offset..(page_offset + thislen)];
                    dest.copy_from_slice(&bytes[count..(count + thislen)]);
                } else {
                    let page = Page::new(alloc_frame(
                        FrameAllocFlags::KERNEL
                            | FrameAllocFlags::WAIT_OK
                            | FrameAllocFlags::ZEROED,
                    ));
                    let page = PageRef::new(Arc::new(page), 0, 1);
                    let dest = &mut page.as_mut_slice()[page_offset..(page_offset + thislen)];
                    dest.copy_from_slice(&bytes[count..(count + thislen)]);
                    obj_page_tree.add_page(page_number, page, None);
                }

                offset += thislen;
                count += thislen;
            }
            if self.use_pager() {
                crate::pager::sync_object(self.id);
            }
        }
    }

    pub fn map_phys(&self, start: PhysAddr, end: PhysAddr, ct: CacheType) {
        let pn_start = PageNumber::from_address(VirtAddr::new(MMIO_OFFSET as u64).unwrap()); //TODO: arch-dep
        let nr = (end.raw() - start.raw()) as usize / PageNumber::PAGE_SIZE;
        for i in 0..nr {
            let pn = pn_start.offset(i);
            let addr = start.offset(i * PageNumber::PAGE_SIZE).unwrap();
            let page = Page::new_wired(addr, PageNumber::PAGE_SIZE, ct);
            let page = PageRef::new(Arc::new(page), 0, 1);
            self.add_page(pn, page, None);
        }
    }
}
