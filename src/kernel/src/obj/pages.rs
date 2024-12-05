use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use twizzler_abi::device::{CacheType, MMIO_OFFSET};

use super::{Object, PageNumber};
use crate::{
    arch::memory::{frame::FRAME_SIZE, phys_to_virt},
    memory::{
        frame::{self, free_frame, FrameRef, PhysicalFrameFlags},
        PhysAddr, VirtAddr,
    },
};

/// An object page can be either a physical frame (allocatable memory) or a static physical address
/// (wired). This will likely be overhauled soon.
#[derive(Debug)]
enum FrameOrWired {
    Frame(FrameRef),
    Wired(PhysAddr),
}

#[derive(Debug)]
pub struct Page {
    frame: FrameOrWired,
    cache_type: CacheType,
}

pub type PageRef = Arc<Page>;

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        match self.frame {
            FrameOrWired::Frame(f) => {
                free_frame(f);
            }
            // Note: this could be a wired, but freeable page (see kernel quick control objects).
            FrameOrWired::Wired(_) => todo!(),
        }
    }
}

impl Page {
    // TODO: we should have a way of allocating non-zero pages, for pages that will be immediately
    // overwritten.
    pub fn new() -> Self {
        Self {
            frame: FrameOrWired::Frame(frame::alloc_frame(PhysicalFrameFlags::ZEROED)),
            cache_type: CacheType::WriteBack,
        }
    }

    pub fn new_wired(pa: PhysAddr, cache_type: CacheType) -> Self {
        Self {
            frame: FrameOrWired::Wired(pa),
            cache_type,
        }
    }

    pub fn as_virtaddr(&self) -> VirtAddr {
        phys_to_virt(self.physical_address())
    }

    pub fn as_slice(&self) -> &[u8] {
        let len = match self.frame {
            FrameOrWired::Frame(f) => f.size(),
            FrameOrWired::Wired(_) => FRAME_SIZE,
        };
        unsafe { core::slice::from_raw_parts(self.as_virtaddr().as_ptr(), len) }
    }

    pub unsafe fn get_mut_to_val<T>(&self, offset: usize) -> *mut T {
        /* TODO: enforce alignment and size of offset */
        /* TODO: once we start optimizing frame zeroing, we need to make the frame as non-zeroed
         * here */
        let va = self.as_virtaddr();
        let bytes = va.as_mut_ptr::<u8>();
        bytes.add(offset) as *mut T
    }

    pub fn as_mut_slice(&self) -> &mut [u8] {
        let len = match self.frame {
            FrameOrWired::Frame(f) => f.size(),
            FrameOrWired::Wired(_) => FRAME_SIZE,
        };
        unsafe { core::slice::from_raw_parts_mut(self.as_virtaddr().as_mut_ptr(), len) }
    }

    pub fn physical_address(&self) -> PhysAddr {
        match self.frame {
            FrameOrWired::Frame(f) => f.start_address(),
            FrameOrWired::Wired(p) => p,
        }
    }

    pub fn copy_page(&self) -> Self {
        let new_frame = frame::alloc_frame(PhysicalFrameFlags::empty());
        match self.frame {
            FrameOrWired::Frame(f) => new_frame.copy_contents_from(f),
            FrameOrWired::Wired(p) => new_frame.copy_contents_from_physaddr(p),
        }
        Self {
            frame: FrameOrWired::Frame(new_frame),
            // TODO: maybe this should default to write-back instead?
            cache_type: self.cache_type,
        }
    }

    pub fn cache_type(&self) -> CacheType {
        self.cache_type
    }
}

impl Object {
    pub unsafe fn write_val_and_signal<T>(&self, offset: usize, val: T, wakeup_count: usize) {
        {
            let mut obj_page_tree = self.lock_page_tree();
            let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
            let page_offset = offset % PageNumber::PAGE_SIZE;

            if let Some((page, _)) = obj_page_tree.get_page(page_number, true) {
                let t = page.get_mut_to_val::<T>(page_offset);
                *t = val;
            } else {
                let page = Page::new();
                obj_page_tree.add_page(page_number, page);
                drop(obj_page_tree);
                self.write_val_and_signal(offset, val, wakeup_count);
                return;
            }
            drop(obj_page_tree);
        }
        self.wakeup_word(offset, wakeup_count);
        crate::syscall::sync::requeue_all();
    }

    pub unsafe fn read_atomic_u64(&self, offset: usize) -> u64 {
        let mut obj_page_tree = self.lock_page_tree();
        let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
        let page_offset = offset % PageNumber::PAGE_SIZE;

        if let Some((page, _)) = obj_page_tree.get_page(page_number, true) {
            let t = page.get_mut_to_val::<AtomicU64>(page_offset);
            logln!("rau64: {:?}", page.physical_address());
            (*t).load(Ordering::SeqCst)
        } else {
            let page = Page::new();
            obj_page_tree.add_page(page_number, page);
            drop(obj_page_tree);
            self.read_atomic_u64(offset)
        }
    }

    pub unsafe fn read_atomic_u32(&self, offset: usize) -> u32 {
        let mut obj_page_tree = self.lock_page_tree();
        let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
        let page_offset = offset % PageNumber::PAGE_SIZE;

        if let Some((page, _)) = obj_page_tree.get_page(page_number, true) {
            let t = page.get_mut_to_val::<AtomicU32>(page_offset);
            (*t).load(Ordering::SeqCst)
        } else {
            let page = Page::new();
            obj_page_tree.add_page(page_number, page);
            drop(obj_page_tree);
            self.read_atomic_u32(offset)
        }
    }

    pub fn write_base<T>(&self, info: &T) {
        let mut offset = FRAME_SIZE;
        unsafe {
            let mut obj_page_tree = self.lock_page_tree();
            let bytes = info as *const T as *const u8;
            let len = core::mem::size_of::<T>();
            let bytes = core::slice::from_raw_parts(bytes, len);
            let mut count = 0;
            while count < len {
                let page_number = PageNumber::from_address(VirtAddr::new(offset as u64).unwrap());
                //let page_offset = offset % PageNumber::PAGE_SIZE;

                let thislen = core::cmp::min(0x1000, len - count);

                if let Some((page, _)) = obj_page_tree.get_page(page_number, true) {
                    let dest = &mut page.as_mut_slice()[0..thislen];
                    dest.copy_from_slice(&bytes[count..(count + thislen)]);
                    //let t = page.get_mut_to_val::<T>(page_offset);
                    //(t as *mut T).copy_from(info as *const T, 1);
                } else {
                    let page = Page::new();
                    let dest = &mut page.as_mut_slice()[0..thislen];
                    dest.copy_from_slice(&bytes[count..(count + thislen)]);
                    //let t = page.get_mut_to_val::<T>(page_offset);
                    //(t as *mut T).copy_from(info as *const T, 1);
                    obj_page_tree.add_page(page_number, page);
                }

                offset += thislen;
                count += thislen;
            }
        }
    }

    pub fn map_phys(&self, start: PhysAddr, end: PhysAddr, ct: CacheType) {
        let pn_start = PageNumber::from_address(VirtAddr::new(MMIO_OFFSET as u64).unwrap()); //TODO: arch-dep
        let nr = (end.raw() - start.raw()) as usize / PageNumber::PAGE_SIZE;
        for i in 0..nr {
            let pn = pn_start.offset(i);
            let addr = start.offset(i * PageNumber::PAGE_SIZE).unwrap();
            let page = Page::new_wired(addr, ct);
            self.add_page(pn, page);
        }
    }
}
