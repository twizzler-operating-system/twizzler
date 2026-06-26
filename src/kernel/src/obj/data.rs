use core::{
    panic,
    sync::atomic::{AtomicU32, AtomicU64},
};

use twizzler_abi::meta::MetaInfo;
use twizzler_rt_abi::error::{ResourceError, TwzError};

use crate::{
    memory::{
        frame::{Frame, FrameRef, PHYS_LEVEL_LAYOUTS, max_level_for_addr},
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
            pt = self.ensure_in_core(pt, pn, 1, &mut pager_was_used)?;
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

    pub fn set_bytes(&self, offset: usize, len: usize, val: u8) -> Result<(), TwzError> {
        let mut offset = offset;
        let mut len = len;
        while len > 0 {
            let written = self.with_frame(
                offset,
                FindFrameFlags::POPULATE | FindFrameFlags::WRITE,
                |po, frame| {
                    let len = core::cmp::min(len, frame.size() - po);
                    let ptr = unsafe { frame.virtaddr().as_mut_ptr::<u8>().byte_add(po) };
                    unsafe {
                        core::ptr::write_bytes(ptr, val, len);
                    }
                    len
                },
            )?;
            offset += written;
            len -= written;
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

    pub fn ensure_both_in_core<'a>(
        &'a self,
        other: &'a Object,
        mut self_guard: LockGuard<'a, ObjectPageTable>,
        mut other_guard: LockGuard<'a, ObjectPageTable>,
        self_page: PageNumber,
        other_page: PageNumber,
        page_count: usize,
        pager_was_used: &mut bool,
    ) -> Result<
        (
            LockGuard<'a, ObjectPageTable>,
            LockGuard<'a, ObjectPageTable>,
        ),
        TwzError,
    > {
        if self_guard
            .get_frame(self_page.as_byte_offset() as u64)
            .is_some()
            && other_guard
                .get_frame(other_page.as_byte_offset() as u64)
                .is_some()
        {
            return Ok((self_guard, other_guard));
        }

        if self.use_pager() || other.use_pager() {
            todo!()
        }

        drop(self_guard);
        drop(other_guard);

        let mut alloc = FrameAllocator::new(
            FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let self_frame = alloc.try_allocate().ok_or(ResourceError::OutOfMemory)?;
        let other_frame = alloc.try_allocate().ok_or(ResourceError::OutOfMemory)?;

        let (mut self_guard, mut other_guard) = crate::utils::lock_two(&self.tables, &other.tables);

        if !self_guard.map_page(self_page.as_byte_offset() as u64, self_frame) {
            alloc.abort([self_frame, other_frame]);
            return Err(TwzError::INVALID_ARGUMENT);
        }
        if !other_guard.map_page(other_page.as_byte_offset() as u64, other_frame) {
            alloc.abort([self_frame, other_frame]);
            return Err(TwzError::INVALID_ARGUMENT);
        }

        Ok((self_guard, other_guard))
    }

    pub fn ensure_in_core<'a>(
        &'a self,
        mut guard: LockGuard<'a, ObjectPageTable>,
        page: PageNumber,
        page_count: usize,
        pager_was_used: &mut bool,
    ) -> Result<LockGuard<'a, ObjectPageTable>, TwzError> {
        if guard.get_frame(page.as_byte_offset() as u64).is_some() {
            return Ok(guard);
        }

        if self.use_pager() {
            return self.ensure_in_core_pager(guard, page, page_count, pager_was_used);
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
        page_count: usize,
        pager_was_used: &mut bool,
    ) -> Result<LockGuard<'a, ObjectPageTable>, TwzError> {
        todo!()
    }

    pub fn direct_copy(
        &self,
        dst: &Object,
        src_offset: usize,
        dst_offset: usize,
        len: usize,
    ) -> Result<(), TwzError> {
        if len == 0 {
            return Ok(());
        }
        let mut src_offset = src_offset;
        let mut dst_offset = dst_offset;
        let mut len = len;

        let (mut self_pt, mut dst_pt) = crate::utils::lock_two(&self.tables, &dst.tables);

        (self_pt, dst_pt) = self.ensure_both_in_core(
            dst,
            self_pt,
            dst_pt,
            PageNumber::from_offset(src_offset),
            PageNumber::from_offset(dst_offset),
            len / PageNumber::PAGE_SIZE,
            &mut false,
        )?;

        while len > 0 {
            self_pt
                .with_frame(
                    src_offset as u64,
                    FindFrameFlags::empty(),
                    |src_frame_offset, self_frame| {
                        dst_pt.with_frame(
                            dst_offset as u64,
                            FindFrameFlags::WRITE | FindFrameFlags::POPULATE,
                            |dst_frame_offset, dst_frame| {
                                let dst_frame = dst_frame.ok_or(TwzError::INVALID_ARGUMENT)?;
                                let dst_ptr = unsafe {
                                    dst_frame
                                        .virtaddr()
                                        .as_mut_ptr::<u8>()
                                        .byte_add(dst_frame_offset)
                                };
                                let dst_frame_rem = dst_frame.size() - dst_frame_offset;

                                let op_len = if let Some(src_frame) = self_frame {
                                    let src_frame_rem = src_frame.size() - src_frame_offset;
                                    let copy_len = core::cmp::min(
                                        len,
                                        core::cmp::min(src_frame_rem, dst_frame_rem),
                                    );
                                    let src_ptr = unsafe {
                                        src_frame
                                            .virtaddr()
                                            .as_ptr::<u8>()
                                            .byte_add(src_frame_offset)
                                    };
                                    if !src_frame.is_zeroed() || !dst_frame.is_zeroed() {
                                        unsafe {
                                            if src_frame.virtaddr() != dst_frame.virtaddr() {
                                                core::ptr::copy_nonoverlapping(
                                                    src_ptr, dst_ptr, copy_len,
                                                );
                                            } else {
                                                core::ptr::copy(src_ptr, dst_ptr, copy_len);
                                            }
                                        }
                                    }
                                    copy_len
                                } else {
                                    // Zero destination if source is not mapped.
                                    let zero_len = core::cmp::min(len, dst_frame_rem);
                                    unsafe {
                                        core::ptr::write_bytes(dst_ptr, 0, zero_len);
                                    }
                                    zero_len
                                };

                                src_offset += op_len;
                                dst_offset += op_len;
                                len -= op_len;
                                Ok::<(), TwzError>(())
                            },
                        )
                    },
                )
                .flatten()?;
        }

        Ok(())
    }

    pub fn cow_copy(
        &self,
        dst: &Object,
        src_offset: usize,
        dst_offset: usize,
        len: usize,
    ) -> Result<(), TwzError> {
        if len == 0 {
            return Ok(());
        }

        assert!(src_offset.is_multiple_of(PHYS_LEVEL_LAYOUTS[0].size()));
        assert!(dst_offset.is_multiple_of(PHYS_LEVEL_LAYOUTS[0].size()));
        assert!(len.is_multiple_of(PHYS_LEVEL_LAYOUTS[0].size()));

        let (mut self_pt, mut dst_pt) = crate::utils::lock_two(&self.tables, &dst.tables);

        // TODO: invalidate src and dst.

        let src_level = max_level_for_addr(src_offset).ok_or(TwzError::INVALID_ARGUMENT)?;
        let dst_level = max_level_for_addr(src_offset).ok_or(TwzError::INVALID_ARGUMENT)?;
        let level = core::cmp::min(src_level, dst_level);
        self_pt.split_to_level(src_offset as u64, level)?;
        self_pt.split_to_level((src_offset + len) as u64, level)?;

        self_pt.setup_cow_range(&mut *dst_pt, src_offset as u64, dst_offset as u64, len)?;

        Ok(())
    }

    pub fn copy_range(
        &self,
        dst: &Object,
        src_offset: usize,
        dst_offset: usize,
        len: usize,
    ) -> Result<(), TwzError> {
        let min_align = PHYS_LEVEL_LAYOUTS[0].size();
        let pre_copy = src_offset % min_align;
        let pre_copy_dst = dst_offset % min_align;

        if pre_copy_dst != pre_copy {
            log::warn!(
                "copy_range: src offset {} and dst offset {} are not aligned to the same boundary ({} vs {})",
                src_offset,
                dst_offset,
                pre_copy,
                pre_copy_dst
            );
            return self.direct_copy(dst, src_offset, dst_offset, len);
        }

        if pre_copy != 0 {
            let pre_len = core::cmp::min(len, min_align - pre_copy);
            self.direct_copy(dst, src_offset, dst_offset, pre_len)?;
            return dst.copy_range(
                self,
                src_offset + pre_len,
                dst_offset + pre_len,
                len - pre_len,
            );
        }

        let post_copy = len % min_align;
        if post_copy != 0 {
            let post_len = post_copy;
            self.direct_copy(
                dst,
                src_offset + len - post_len,
                dst_offset + len - post_len,
                post_len,
            )?;
            return dst.copy_range(self, src_offset, dst_offset, len - post_len);
        }

        self.cow_copy(dst, src_offset, dst_offset, len)
    }

    pub fn zero_range(&self, offset: usize, len: usize) -> Result<(), TwzError> {
        if len == 0 {
            return Ok(());
        }
        let pre_zero = offset % PHYS_LEVEL_LAYOUTS[0].size();
        if pre_zero != 0 {
            let pre_len = core::cmp::min(len, PHYS_LEVEL_LAYOUTS[0].size() - pre_zero);
            self.set_bytes(offset, pre_len, 0)?;
            return self.zero_range(offset + pre_len, len - pre_len);
        }

        let post_zero = len % PHYS_LEVEL_LAYOUTS[0].size();
        if post_zero != 0 {
            let post_len = post_zero;
            self.set_bytes(offset + len - post_len, post_len, 0)?;
            return self.zero_range(offset, len - post_len);
        }

        let mut pt = self.lock_page_tables();
        pt.setup_zero_range(offset as u64, len)?;

        Ok(())
    }
}
