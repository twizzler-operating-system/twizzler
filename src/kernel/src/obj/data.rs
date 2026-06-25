use core::{
    panic,
    sync::atomic::{AtomicU32, AtomicU64},
};

use twizzler_abi::meta::MetaInfo;
use twizzler_rt_abi::error::{ResourceError, TwzError};

use crate::{
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS},
        tracker::{FrameAllocFlags, FrameAllocator},
    },
    mutex::LockGuard,
    obj::{
        Object, PageNumber,
        pagetables::{FindFrameFlags, ObjectPageTable},
    },
    pager::ensure_in_core,
};

enum ZeroOrFrame {
    Zeroed(usize),
    Frame(usize, FrameRef),
}

impl Object {
    fn do_with_frame<R>(
        &self,
        offset: usize,
        flags: FindFrameFlags,
        f: impl FnOnce(usize, Option<FrameRef>) -> R,
    ) -> Result<R, TwzError> {
        let pn = PageNumber::from_offset(offset);
        let mut pt = self.lock_page_tables();
        if flags.contains(FindFrameFlags::POPULATE) {
            let mut pager_was_used = false;
            pt = self.ensure_in_core(pt, pn, &mut pager_was_used)?;
        }
        pt.with_frame(offset as u64, flags, |frame_offset, frame| {
            f(frame_offset, frame)
        })
    }

    fn with_frame<R>(
        &self,
        offset: usize,
        flags: FindFrameFlags,
        f: impl FnOnce(usize, FrameRef) -> R,
    ) -> Result<R, TwzError> {
        self.do_with_frame(
            offset,
            flags | FindFrameFlags::POPULATE,
            |frame_offset, frame| {
                if let Some(frame) = frame {
                    let po = offset - frame_offset;
                    f(po, frame)
                } else {
                    panic!(
                        "with_page called on object {} at offset {} but no frame was found",
                        self.id(),
                        offset
                    );
                }
            },
        )
    }

    fn with_optional_frame<R>(
        &self,
        offset: usize,
        f: impl FnOnce(ZeroOrFrame) -> R,
    ) -> Result<R, TwzError> {
        self.do_with_frame(offset, FindFrameFlags::empty(), |frame_offset, frame| {
            if let Some(frame) = frame {
                let po = offset - frame_offset;
                f(ZeroOrFrame::Frame(po, frame))
            } else {
                let zlen = offset % PHYS_LEVEL_LAYOUTS[0].size();
                f(ZeroOrFrame::Zeroed(zlen))
            }
        })
    }

    pub fn read_meta(&self) -> Option<MetaInfo> {
        self.read_at(PageNumber::meta_page().as_byte_offset()).ok()
    }

    pub fn write_meta(&self, meta: MetaInfo) -> bool {
        self.write_at(&meta, PageNumber::meta_page().as_byte_offset())
            .is_ok()
    }

    pub fn with_ref<R, P>(&self, offset: usize, f: impl FnOnce(&P) -> R) -> Result<R, TwzError> {
        assert!(offset.is_multiple_of(align_of::<P>()));
        self.with_frame(
            offset,
            FindFrameFlags::POPULATE | FindFrameFlags::WRITE,
            |po, frame| {
                assert!(po.is_multiple_of(align_of::<P>()));
                assert!(po + core::mem::size_of::<P>() <= frame.size());
                let ptr = unsafe { frame.virtaddr().as_ptr::<P>().byte_add(po) };
                f(unsafe { &*ptr })
            },
        )
    }

    pub fn read_atomic_64(&self, offset: usize) -> Result<u64, TwzError> {
        let aoffset = offset & !(core::mem::size_of::<u64>() - 1);
        if aoffset != offset {
            log::warn!(
                "unaligned atomic read at offset {} (aligned to {}) in object {}",
                offset,
                aoffset,
                self.id()
            );
        }
        self.with_ref(aoffset, |ptr: &AtomicU64| -> u64 {
            ptr.load(core::sync::atomic::Ordering::SeqCst)
        })
    }

    pub fn swap_atomic_64(&self, offset: usize, val: u64) -> Result<u64, TwzError> {
        let aoffset = offset & !(core::mem::size_of::<u64>() - 1);
        if aoffset != offset {
            log::warn!(
                "unaligned atomic swap at offset {} (aligned to {}) in object {}",
                offset,
                aoffset,
                self.id()
            );
        }
        self.with_ref(aoffset, |ptr: &AtomicU64| -> u64 {
            ptr.swap(val, core::sync::atomic::Ordering::SeqCst)
        })
    }

    pub fn read_atomic_32(&self, offset: usize) -> Result<u32, TwzError> {
        let aoffset = offset & !(core::mem::size_of::<u32>() - 1);
        if aoffset != offset {
            log::warn!(
                "unaligned atomic read at offset {} (aligned to {}) in object {}",
                offset,
                aoffset,
                self.id()
            );
        }
        self.with_ref(aoffset, |ptr: &AtomicU32| -> u32 {
            ptr.load(core::sync::atomic::Ordering::SeqCst)
        })
    }

    pub fn swap_atomic_32(&self, offset: usize, val: u32) -> Result<u32, TwzError> {
        let aoffset = offset & !(core::mem::size_of::<u32>() - 1);
        if aoffset != offset {
            log::warn!(
                "unaligned atomic swap at offset {} (aligned to {}) in object {}",
                offset,
                aoffset,
                self.id()
            );
        }
        self.with_ref(aoffset, |ptr: &AtomicU32| -> u32 {
            ptr.swap(val, core::sync::atomic::Ordering::SeqCst)
        })
    }

    pub fn write_at<T>(&self, val: &T, offset: usize) -> Result<(), TwzError> {
        self.write_bytes(
            val as *const T as *const u8,
            core::mem::size_of::<T>(),
            offset,
        )
    }

    pub fn read_at<T>(&self, offset: usize) -> Result<T, TwzError> {
        let mut val = core::mem::MaybeUninit::<T>::uninit();
        self.read_bytes(
            unsafe {
                core::slice::from_raw_parts_mut(
                    val.as_mut_ptr() as *mut u8,
                    core::mem::size_of::<T>(),
                )
            },
            offset,
        )?;
        Ok(unsafe { val.assume_init() })
    }

    pub fn read_bytes(&self, slice: &mut [u8], offset: usize) -> Result<(), TwzError> {
        let mut offset = offset;
        let mut slice = slice;
        while !slice.is_empty() {
            let len = self.with_optional_frame(offset, |zof| match zof {
                ZeroOrFrame::Zeroed(zlen) => {
                    let len = core::cmp::min(slice.len(), zlen);
                    slice[..len].fill(0);
                    len
                }
                ZeroOrFrame::Frame(po, frame) => {
                    let len = core::cmp::min(slice.len(), frame.size() - po);
                    let ptr = unsafe { frame.virtaddr().as_ptr::<u8>().byte_add(po) };
                    unsafe {
                        core::ptr::copy_nonoverlapping(ptr, slice.as_mut_ptr(), len);
                    }
                    len
                }
            })?;
            slice = &mut slice[len..];
            offset += len;
        }

        Ok(())
    }

    pub fn write_bytes(&self, ptr: *const u8, len: usize, offset: usize) -> Result<(), TwzError> {
        let mut offset = offset;
        let mut slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        while !slice.is_empty() {
            let len = self.with_frame(
                offset,
                FindFrameFlags::POPULATE | FindFrameFlags::WRITE,
                |po, frame| {
                    let len = core::cmp::min(slice.len(), frame.size() - po);
                    let ptr = unsafe { frame.virtaddr().as_mut_ptr::<u8>().byte_add(po) };
                    unsafe {
                        core::ptr::copy_nonoverlapping(slice.as_ptr(), ptr, len);
                    }
                    len
                },
            )?;
            slice = &slice[len..];
            offset += len;
        }

        Ok(())
    }

    pub fn read_base<T>(&self) -> Result<T, TwzError> {
        self.read_at(PageNumber::base_page().as_byte_offset())
    }

    pub fn write_base<T>(&self, val: &T) -> Result<(), TwzError> {
        self.write_at(val, PageNumber::base_page().as_byte_offset())
    }

    pub fn try_write_val_and_signal(
        &self,
        offset: usize,
        val: u64,
        wake_count: usize,
    ) -> Result<(), TwzError> {
        self.swap_atomic_64(offset, val)?;
        self.wakeup_word(offset, wake_count);
        crate::syscall::sync::requeue_all();
        Ok(())
    }

    pub fn ensure_in_core<'a>(
        &'a self,
        mut guard: LockGuard<'a, ObjectPageTable>,
        page: PageNumber,
        pager_was_used: &mut bool,
    ) -> Result<LockGuard<'a, ObjectPageTable>, TwzError> {
        if guard.get_frame(page.as_byte_offset() as u64).is_some() {
            return Ok(guard);
        }

        if self.use_pager() {
            return self.ensure_in_core_pager(guard, page, pager_was_used);
        }
        drop(guard);
        let mut alloc = FrameAllocator::new(
            FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let frame = alloc.try_allocate().ok_or(ResourceError::OutOfMemory)?;

        guard = self.lock_page_tables();

        if !guard.map_page(page.as_byte_offset() as u64, frame) {
            alloc.abort([frame]);
        }

        Ok(guard)
    }

    pub fn ensure_in_core_pager<'a>(
        &'a self,
        guard: LockGuard<'a, ObjectPageTable>,
        page: PageNumber,
        pager_was_used: &mut bool,
    ) -> Result<LockGuard<'a, ObjectPageTable>, TwzError> {
        todo!()
    }
}
