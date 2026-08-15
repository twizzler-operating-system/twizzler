use alloc::vec::Vec;
use core::{
    panic,
    sync::atomic::{AtomicU32, AtomicU64},
};

use twizzler_abi::{meta::MetaInfo, pager::PagerFlags, syscall::PinnedPage};
use twizzler_rt_abi::error::{ResourceError, TwzError};

use crate::{
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, max_level_for_addr},
        tracker::{FrameAllocFlags, FrameAllocator},
    },
    obj::{
        Object, ObjectRef, PageNumber, PtGuard,
        pagetables::{FindFrameFlags, ObjectPageTable},
    },
    thread::current_thread_ref,
};

enum ZeroOrFrame {
    Zeroed(usize),
    Frame(usize, FrameRef),
}

/// Install a 2 MiB frame on the first fault into an empty 2 MiB region of an anonymous object.
///
/// **Off**, measured. The bet is that one fault beats 512, but it is paid up front and in full: the
/// frame is zeroed synchronously on the faulting thread, and a large frame always comes from a
/// never-touched buddy region, so the zero is really ~512 host page faults. Measured at ~1.5 ms
/// each. A thread stack touches a handful of pages out of its whole span, so for that shape the
/// bet loses badly.
///
/// A/B over one boot of the default workload, at the same endpoint:
///
/// | | on | off |
/// |---|---|---|
/// | page faults | 1,680 | 19,150 |
/// | total fault time | 744 ms | 600 ms |
/// | large frames zeroed | 357 / 644 ms | 63 / 86 ms |
/// | small frames zeroed | 87,244 / 9 ms | 105,515 / 63 ms |
///
/// So 11x the faults still comes out ahead, because the zeroing it avoids costs far more than the
/// extra faults do. Left as a switch because that ranking is workload-dependent: something that
/// densely touches large regions pays the zeroing either way and would rather have one fault.
///
/// Only the anonymous path. The pager path builds its own large pages out of read-ahead, and its
/// 2 MiB alignment is load-bearing in `pager_compl_handle_page_data`.
const TRY_LARGE_ANON_PAGES: bool = false;

/// Whether a volatile object's first touch of an empty region actually gets a large frame.
///
/// The allocation below is a non-waiting `try_allocate` at level 1, so it fails silently and falls
/// back to filling the region 4 KiB at a time -- which is how regions end up merely *promotable*
/// rather than large (`promote.md`). Nothing distinguished "never attempted" from "attempted and
/// refused" before this.
mod largealloc {
    use core::sync::atomic::{AtomicU64, Ordering};

    static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
    static FAILED: AtomicU64 = AtomicU64::new(0);

    pub fn record(ok: bool) {
        if !ok {
            FAILED.fetch_add(1, Ordering::Relaxed);
        }
        let n = ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            log::info!(
                "LARGEALLOC: {} up-front large-frame allocations, {} failed",
                n,
                FAILED.load(Ordering::Relaxed),
            );
        }
    }
}

impl Object {
    fn do_with_frame<R>(
        self: &ObjectRef,
        offset: usize,
        flags: FindFrameFlags,
        f: impl FnOnce(usize, Option<FrameRef>) -> R,
    ) -> Result<R, TwzError> {
        let pn = PageNumber::from_offset(offset);
        let mut pt = self.lock_page_tables();
        log::trace!(
            "do_with_frame: offset {:x}, page {:x}, flags {:?}",
            offset,
            pn.as_byte_offset(),
            flags
        );
        if flags.contains(FindFrameFlags::POPULATE) || self.use_pager() {
            pt = self.ensure_in_core(pt, pn, 1, &mut false, &mut false)?;
        }
        let mut did_cow = false;
        let r = pt.with_frame(offset as u64, flags, &mut did_cow, |frame_offset, frame| {
            f(frame_offset, frame)
        });
        r
    }

    fn with_frame<R>(
        self: &ObjectRef,
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
                    log::debug!(
                        "==> with_frame: offset {:x}, frame_offset {:x}, po {:x}, frame {:x}",
                        offset,
                        frame_offset,
                        po,
                        frame.start_address().raw()
                    );
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
        self: &ObjectRef,
        offset: usize,
        f: impl FnOnce(ZeroOrFrame) -> R,
    ) -> Result<R, TwzError> {
        self.do_with_frame(offset, FindFrameFlags::empty(), |frame_offset, frame| {
            if let Some(frame) = frame {
                let po = offset - frame_offset;
                f(ZeroOrFrame::Frame(po, frame))
            } else {
                let zlen: usize = offset - (offset % PHYS_LEVEL_LAYOUTS[0].size());
                f(ZeroOrFrame::Zeroed(zlen))
            }
        })
    }

    pub fn read_meta(self: &ObjectRef) -> Option<MetaInfo> {
        self.read_at(PageNumber::meta_page().as_byte_offset()).ok()
    }

    pub fn write_meta(self: &ObjectRef, meta: MetaInfo) -> bool {
        let ok = self
            .write_at(&meta, PageNumber::meta_page().as_byte_offset())
            .inspect_err(|e| log::warn!("failed to write metadata: {}", e))
            .is_ok();
        if ok {
            // The kernel chose these fields, so it already knows what `check_id` would find.
            self.note_written_meta(&meta);
        }
        ok
    }

    pub fn with_ref<R, P>(
        self: &ObjectRef,
        offset: usize,
        f: impl FnOnce(&P) -> R,
    ) -> Result<R, TwzError> {
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

    pub fn read_atomic_64(self: &ObjectRef, offset: usize) -> Result<u64, TwzError> {
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

    pub fn swap_atomic_64(self: &ObjectRef, offset: usize, val: u64) -> Result<u64, TwzError> {
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

    pub fn read_atomic_32(self: &ObjectRef, offset: usize) -> Result<u32, TwzError> {
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

    pub fn swap_atomic_32(self: &ObjectRef, offset: usize, val: u32) -> Result<u32, TwzError> {
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

    pub fn write_at<T>(self: &ObjectRef, val: &T, offset: usize) -> Result<(), TwzError> {
        self.write_bytes(
            val as *const T as *const u8,
            core::mem::size_of::<T>(),
            offset,
        )
    }

    pub fn read_at<T>(self: &ObjectRef, offset: usize) -> Result<T, TwzError> {
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

    pub fn read_bytes(self: &ObjectRef, slice: &mut [u8], offset: usize) -> Result<(), TwzError> {
        log::trace!(
            "read_bytes: reading {} bytes at offset {:x} in object {}",
            slice.len(),
            offset,
            self.id()
        );
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

    pub fn write_bytes(
        self: &ObjectRef,
        ptr: *const u8,
        len: usize,
        offset: usize,
    ) -> Result<(), TwzError> {
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

    pub fn set_bytes(self: &ObjectRef, offset: usize, len: usize, val: u8) -> Result<(), TwzError> {
        let mut offset = offset;
        let mut len = len;
        while len > 0 {
            let written = self.with_frame(
                offset,
                FindFrameFlags::POPULATE | FindFrameFlags::WRITE,
                |po, frame| {
                    let len = core::cmp::min(len, frame.size() - po);
                    log::debug!(
                        "set bytes: writing {:x} bytes at offset {:x} in frame {:x} (po = {:x})",
                        len,
                        offset,
                        frame.start_address().raw(),
                        po
                    );
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

    pub fn read_base<T>(self: &ObjectRef) -> Result<T, TwzError> {
        self.read_at(PageNumber::base_page().as_byte_offset())
    }

    pub fn write_base<T>(self: &ObjectRef, val: &T) -> Result<(), TwzError> {
        self.write_at(val, PageNumber::base_page().as_byte_offset())
    }

    pub fn try_write_val_and_signal(
        self: &ObjectRef,
        offset: usize,
        val: u64,
        wake_count: usize,
    ) -> Result<(), TwzError> {
        self.swap_atomic_64(offset, val)?;
        self.wakeup_word(offset, wake_count);
        crate::syscall::sync::requeue_all();
        Ok(())
    }

    pub fn pin(
        self: &ObjectRef,
        page: PageNumber,
        count: usize,
    ) -> Result<(Vec<PinnedPage>, u32), TwzError> {
        let mut pages = Vec::new();
        for i in 0..count {
            let page_offset = page.offset(i).as_byte_offset() as u64;
            self.with_frame(
                page_offset as usize,
                FindFrameFlags::POPULATE | FindFrameFlags::WRITE,
                // `po` is the offset of this page within the frame backing it, which is only zero
                // when that frame is a 4 KiB one. A large page backs 512 of these offsets and
                // reports its region base for all of them, so the page's own address is the base
                // plus `po`.
                |po, frame| {
                    pages.push(PinnedPage::new(frame.start_address().raw() + po as u64));
                },
            )?;
        }
        Ok((pages, 0))
    }

    #[track_caller]
    pub fn ensure_in_core<'a>(
        self: &'a ObjectRef,
        mut guard: PtGuard<'a>,
        mut page: PageNumber,
        mut page_count: usize,
        pager_was_used: &mut bool,
        all_were_present: &mut bool,
    ) -> Result<PtGuard<'a>, TwzError> {
        let first_is_present = guard.get_frame(page.as_byte_offset() as u64).is_some();
        *all_were_present = true;
        if page_count <= 1 && first_is_present {
            return Ok(guard);
        }
        log::trace!(
            "ensure in core: ensuring {} pages in core for object {} starting at {:x}",
            page_count,
            self.id(),
            page.as_byte_offset()
        );
        if self.use_pager() {
            return self.ensure_in_core_pager(
                guard,
                page,
                page_count,
                pager_was_used,
                all_were_present,
                PagerFlags::empty(),
                false,
            );
        }
        let mut alloc = FrameAllocator::new(
            FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Try to get the frames without giving the caller's lock up. Only *waiting* for memory is
        // unacceptable here -- it would block every other fault on this object -- and there is
        // normally nothing to wait for, so the unconditional drop-and-retake this used to do cost
        // an extra acquisition (~750 ns) on every fill fault to insure against the rare case.
        if !first_is_present && alloc.precharge_nowait(page_count) < page_count {
            drop(guard);
            alloc.precharge(page_count, FrameAllocFlags::WAIT_OK);
            guard = self.lock_page_tables();
        }

        if TRY_LARGE_ANON_PAGES
            && page != PageNumber::meta_page()
            && guard.is_empty_at_level(page.as_byte_offset() as u64, 1)
        {
            let nr_pages_for_large = PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE;
            let large_page = page.align_down(nr_pages_for_large);
            let pre_covered = page - large_page;
            let mut alloc = FrameAllocator::new(FrameAllocFlags::ZEROED, PHYS_LEVEL_LAYOUTS[1]);
            let large_frame = alloc.try_allocate();
            largealloc::record(large_frame.is_some());
            if let Some(large_frame) = large_frame {
                *all_were_present = false;
                guard.map_page(large_page.as_byte_offset() as u64, large_frame)?;
                page = large_page.offset(nr_pages_for_large);
                page_count = page_count.saturating_sub(nr_pages_for_large - pre_covered);

                log::trace!(
                    "mapped large page at offset {:x} in object {} ({} pages remaining)",
                    large_page.as_byte_offset(),
                    self.id(),
                    page_count
                );
            };
        }

        log::debug!(
            "ensure_in_core: ensuring {} pages in core for object {} starting at {:x} {} (from {})",
            page_count,
            self.id(),
            page.as_byte_offset(),
            current_thread_ref().map(|ct| ct.id()).unwrap_or(0),
            core::panic::Location::caller()
        );
        for i in 0..page_count {
            let offset = page.offset(i).as_byte_offset() as u64;
            if guard.is_empty_at_level(offset, 0) {
                log::trace!(
                    "filling frame at offset {:x} in object {}",
                    offset,
                    self.id()
                );
                *all_were_present = false;
                let frame = alloc.try_allocate().ok_or(ResourceError::OutOfMemory)?;
                if let Err(e) = guard.map_page(offset, frame) {
                    log::error!(
                        "failed to map page at offset {:x} in object {}",
                        offset,
                        self.id()
                    );
                    alloc.abort([frame]);
                    return Err(e);
                }
            }
        }

        Ok(guard)
    }

    /// Back `[page, page + count)` with freshly zeroed frames, without involving the pager.
    ///
    /// Only correct for a range the store provably does not hold; see [Object::known_len] for why
    /// the kernel is entitled to decide that, and note the error direction -- being wrong here
    /// means serving zeros over real data.
    fn fill_zero_pages<'a>(
        self: &'a ObjectRef,
        mut guard: PtGuard<'a>,
        page: PageNumber,
        count: usize,
        all_were_present: &mut bool,
    ) -> Result<PtGuard<'a>, TwzError> {
        let mut alloc = FrameAllocator::new(
            FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        for i in 0..count {
            let offset = page.offset(i).as_byte_offset() as u64;
            if !guard.is_empty_at_level(offset, 0) {
                continue;
            }
            *all_were_present = false;
            let frame = alloc.try_allocate().ok_or(ResourceError::OutOfMemory)?;
            if let Err(e) = guard.map_page(offset, frame) {
                alloc.abort([frame]);
                return Err(e);
            }
        }
        Ok(guard)
    }

    /// Whether a fault waits only for the pages it asked for, or for the whole region the widening
    /// below adds around them. See the note at `required` inside [Object::ensure_in_core_pager].
    const SPLIT_ON_REQUIRED: bool = true;

    /// How many large-page regions a touch of an empty region is widened to. See the note at the
    /// clamp in [Object::ensure_in_core_pager]; `2` is the historical behaviour.
    pub(crate) const READAHEAD_REGIONS: usize = 2;

    /// Whether a widened read-ahead range is submitted as one request per 2 MiB region instead of
    /// one contiguous request spanning them all.
    ///
    /// The point is concurrency, not transfer: the pager serves one request on one lane, so a
    /// single 1024-page request cannot use more than one of its workers however many it has. Two
    /// 512-page requests can. Measured against `pagepar`, which is the workload built to have
    /// several page-ins outstanding at once. Set false to restore the single-request shape.
    ///
    /// Measured on: kernel max-outstanding 5 -> 11, `REQSTATS` 3 -> 6, and large-page merges *rose*
    /// from 8-11 to 32 of 32 candidates -- so cutting on region boundaries demonstrably costs no
    /// merges, which is the tradeoff `page_data_request` warns about for arbitrary splits.
    ///
    /// **Off, because on this workload the depth it bought was phantom.** The first attempt raised
    /// depth to 11 and merges to 32/32, but also doubled pages delivered (11k -> 24.9k) -- the
    /// extra pages were holes past EOF, from a short file's second region lying wholly beyond the
    /// object. With that fixed on both sides (the clamp above, and the `start > max_len` case in
    /// `handle_page_data_request_task`) delivery came back to 11.6k and *the depth went with it*:
    /// max-outstanding 11 -> 5, merges 32 -> 8, i.e. exactly the pre-split baseline.
    ///
    /// So the concurrency was the hole requests, and the 32 merges were merges of holes. Once the
    /// widening is correctly trimmed, `pagepar`'s files are smaller than one 2 MiB region and there
    /// is nothing left to split. The mechanism is sound and costs no merges -- worth revisiting for
    /// a workload of multi-region objects -- but it does nothing here, and "on" would imply
    /// otherwise.
    const SPLIT_REQ_PER_REGION: bool = false;

    /// `flags` distinguishes a demand fault from speculation. It changes nothing about which pages
    /// are requested -- the point of driving this path with [PagerFlags::PREFETCH] rather than
    /// hand-rolling a range is that a prefetch then asks for *exactly* what the fault it is trying
    /// to pre-empt would ask for, presence checks and widening included, so the two coalesce in
    /// `InflightManager::add_request` instead of paging the same range twice. It only decides
    /// whether the caller waits.
    pub fn ensure_in_core_pager<'a>(
        self: &'a ObjectRef,
        mut guard: PtGuard<'a>,
        mut page: PageNumber,
        mut page_count: usize,
        pager_was_used: &mut bool,
        all_were_present: &mut bool,
        flags: PagerFlags,
        speculative: bool,
    ) -> Result<PtGuard<'a>, TwzError> {
        *all_were_present = true;
        assert!(self.use_pager());

        // Past the end of the store's data there is nothing to read -- the kernel is the only thing
        // that extends it -- so the pager has nothing to say and the round trip is pure latency.
        // This is what would make appending to a file cost no pager traffic until it is synced.
        //
        // Deliberately only the caller's own range, never the widening below: a small file's
        // widened request runs a thousand pages past EOF, and committing a zeroed frame for each
        // would turn a 64 KB file into 4 MB of resident memory.
        //
        // **Off, because "past the data length" is not "absent from the store".** An object's
        // metadata lives at the *top* of its address range: the meta page at `MAX_SIZE - PAGE`,
        // and the FOT growing *downward* from it (`resolve_fot` reads
        // `meta.cast::<FotEntry>().sub(idx + 1)`). All of that is on disk and all of it is past
        // `known_len`, which describes only the data at the bottom. Excluding the meta page alone
        // still zero-filled the FOT, so every library's foreign-object table read back as zeros
        // and the guest died with `failed to enumerate dependencies for libtwz_rt.so`
        // (`pagerperf.md` 20). Turning this on needs a real bound on where the metadata region
        // starts -- `MetaInfo::fotcount` gives it, at the cost of a meta-page read on the fault
        // path -- not a wider exclusion.
        const ZERO_FILL_PAST_EOF: bool = false;
        if ZERO_FILL_PAST_EOF
            && page != PageNumber::meta_page()
            && self
                .known_len()
                .is_some_and(|len| page.as_byte_offset() as u64 >= len)
        {
            return self.fill_zero_pages(guard, page, page_count, all_were_present);
        }
        // What the caller asked for, captured before the widening below rewrites `page` and
        // `page_count`. Everything the widening adds is speculative: it exists to install a large
        // page and to save later faults, and nothing is blocked on it. Handing it to
        // `ensure_in_core` is what lets the wait end when this range is backed rather than when
        // the whole widened region is (`pagerperf.md` 11).
        //
        // `None` reproduces the old behaviour exactly -- an empty required range on the wire makes
        // the pager serve the request in address order as one segment, and the kernel wait for all
        // of it -- so this is a one-rebuild A/B, in the habit of `PIPELINE_DEPTH`.
        let required = Self::SPLIT_ON_REQUIRED.then_some((page, page_count));
        // The caller's own end, before the widening moves it. The trim below never goes under this.
        let asked_end = page.offset(page_count);
        log::debug!(
            "ensure_in_core_pager: ensuring {} pages in core for object {} starting at {:x}",
            page_count,
            self.id(),
            page.as_byte_offset()
        );

        let nr_pages_for_large = PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE;
        if page != PageNumber::meta_page()
            && guard.is_empty_at_level(page.as_byte_offset() as u64, 1)
        {
            let large_page = page.align_down(nr_pages_for_large);
            let pre_covered = page - large_page;
            page = large_page;
            // Moving the start back to `large_page` needs `pre_covered` more pages to cover the
            // same tail; the clamp is what rounds the whole thing up to the read-ahead window.
            //
            // This used to read `page_count += pre_covered.max(nr_pages_for_large)`, which clamps
            // the wrong operand: `pre_covered` is an offset within the region, so it is always
            // under `nr_pages_for_large` and that `max` is the constant `nr_pages_for_large`. A
            // one-page touch came out at 513 -- one page into the *next* region, which the loop
            // below then rounded up to a whole second one. That is where the 1024-page first-touch
            // request every note about this path describes comes from, against prose that says one
            // region everywhere.
            //
            // `READAHEAD_REGIONS = 2` keeps that window deliberately rather than by accident, and
            // is what the measurements in `pagerperf.md` and `mapperf.md` were taken against. It is
            // not free to lower: object page 0 is never delivered, so a run covering region 0
            // starts at page 1 and fails the 2MB-alignment test in `pager_compl_handle_page_data`.
            // The spill into region 1 is the only reason a first-touch fault installs a large page
            // at all, so 1 trades that -- and half the read-ahead -- for half the transfer.
            page_count =
                (page_count + pre_covered).max(nr_pages_for_large * Self::READAHEAD_REGIONS);
            log::debug!(
                "paging in large page at offset {:x} in object {} ({} pages, {} pages pre-covered)",
                large_page.as_byte_offset(),
                self.id(),
                page_count,
                pre_covered
            );
        }

        let remaining = nr_pages_for_large - (page.num() % nr_pages_for_large);
        if page != PageNumber::meta_page() && remaining > page_count {
            let extra = remaining.min(64);
            page_count = page_count.max(extra);
        }

        // Drop the part of the *speculative* tail that lies past the store's data length.
        //
        // The widening above rounds a touch up to `READAHEAD_REGIONS` whole 2 MiB regions without
        // regard to how long the object is, so a 64 KiB file is asked for 1024 pages. The pager
        // clamps the range to the object's bounds and delivers what exists, which is why this has
        // never been a correctness problem -- but it means the ask is ~4.5x what comes back
        // (50423 pages requested against 11260 delivered on `pagepar`), the ask is what
        // `pages_requested` reports, and it is what every read-amplification number is computed
        // from.
        //
        // Emphatically *not* the reasoning behind [ZERO_FILL_PAST_EOF] above, which is off because
        // it is wrong: "past `known_len`" does not mean "the store has nothing there", since an
        // object's metadata -- the meta page and the FOT growing down from it -- lives at the top
        // of the address range and is entirely past the data length. So this fabricates nothing.
        // All it does is decline to *speculate* past the point where speculation cannot pay.
        //
        // `max(asked_end)` is what makes that safe, and it is the whole guard: the caller's own
        // range survives verbatim however far past `data_end` it reaches, so a fault in the
        // metadata region still asks for exactly the pages it faulted on. This used to bail out
        // entirely in that case, which left 33 widenings a boot un-trimmed at their full 1024
        // pages -- and once `SPLIT_REQ_PER_REGION` cut those into per-region requests, the second
        // region lay wholly past the length and the pager served it as ~512 pages of holes.
        match self.known_len() {
            // Rounded up: a length landing mid-page still has that page's data behind it.
            Some(len) => {
                let data_end =
                    PageNumber::from_offset((len as usize).next_multiple_of(PageNumber::PAGE_SIZE));
                let trimmed = page.offset(page_count).min(data_end).max(asked_end) - page;
                if asked_end > data_end {
                    crate::pager::profile::PAGER_PROFILE.eof_past_end();
                }
                crate::pager::profile::PAGER_PROFILE.eof_clamped(page_count - trimmed);
                page_count = trimmed;
            }
            None => crate::pager::profile::PAGER_PROFILE.eof_no_len(),
        }

        let mut reqs = heapless::Vec::<_, 16>::new();

        let push_reqs =
            |pn: PageNumber, len: usize, reqs: &mut heapless::Vec<(PageNumber, usize), 16>| {
                // A new 2 MiB region starts a new request rather than extending the last one.
                //
                // The widened range is contiguous by construction, so coalescing turned it into a
                // single request covering every region -- and one request is one unit of work for
                // the pager, served by one lane. That is why the submit-all-then-wait split in
                // `ensure_in_core` changed nothing: there was never a second request to overlap
                // with the first.
                //
                // Splitting *here* specifically costs no large-page merges. A merge requires the
                // object page to be 2 MiB-aligned (`pager_compl_handle_page_data`), so a region
                // boundary is the one place a request can be cut without ever landing inside a
                // run that would have merged -- which is what the warning against splitting
                // freely in `page_data_request` is about.
                let starts_region = Self::SPLIT_REQ_PER_REGION
                    && pn
                        .as_byte_offset()
                        .is_multiple_of(PHYS_LEVEL_LAYOUTS[1].size());
                if reqs.is_empty() || starts_region {
                    reqs.push((pn, len)).unwrap();
                } else {
                    let (last_page, last_count) = reqs.last_mut().unwrap();
                    if last_page.offset(*last_count) == pn {
                        *last_count += len;
                    } else {
                        reqs.push((pn, len)).unwrap();
                    }
                }
            };

        while page_count > 0 {
            let done_count = if page
                .as_byte_offset()
                .is_multiple_of(PHYS_LEVEL_LAYOUTS[1].size())
            {
                if guard.is_empty_at_level(page.as_byte_offset() as u64, 1) {
                    push_reqs(
                        page,
                        PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE,
                        &mut reqs,
                    );
                    PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE
                } else {
                    if guard.is_empty_at_level(page.as_byte_offset() as u64, 0) {
                        push_reqs(page, 1, &mut reqs);
                    }
                    1
                }
            } else {
                if guard.is_empty_at_level(page.as_byte_offset() as u64, 0) {
                    push_reqs(page, 1, &mut reqs);
                }
                1
            };

            if !reqs.is_empty() {
                *all_were_present = false;
            }
            page_count = page_count.saturating_sub(done_count);
            page = page.offset(done_count);
            if reqs.is_full() {
                guard = crate::pager::ensure_in_core(
                    self,
                    guard,
                    &reqs,
                    flags,
                    speculative,
                    pager_was_used,
                    required,
                )?;
                reqs.clear();
            }
        }

        if !reqs.is_empty() {
            *all_were_present = false;
            guard = crate::pager::ensure_in_core(
                self,
                guard,
                &reqs,
                flags,
                speculative,
                pager_was_used,
                required,
            )?;
        }

        Ok(guard)
    }

    pub fn direct_copy(
        self: &ObjectRef,
        dst: &ObjectRef,
        src_offset: usize,
        dst_offset: usize,
        len: usize,
    ) -> Result<(), TwzError> {
        if len == 0 {
            return Ok(());
        }
        log::debug!(
            "direct_copy: src_offset {:x}, dst_offset {:x}, len {} ({} => {})",
            src_offset,
            dst_offset,
            len,
            self.id(),
            dst.id()
        );
        let mut src_offset = src_offset;
        let mut dst_offset = dst_offset;
        let mut len = len;
        if self.use_pager() {
            let _ = self.ensure_in_core(
                self.lock_page_tables(),
                PageNumber::from_offset(src_offset),
                (len.saturating_sub(1) / PageNumber::PAGE_SIZE) + 2,
                &mut false,
                &mut false,
            )?;
        }
        if dst.use_pager() {
            let _ = dst.ensure_in_core(
                dst.lock_page_tables(),
                PageNumber::from_offset(dst_offset),
                (len.saturating_sub(1) / PageNumber::PAGE_SIZE) + 2,
                &mut false,
                &mut false,
            )?;
        }
        let (mut self_pt, mut dst_pt) = PtGuard::new_two(&self.tables, &dst.tables);

        log::debug!(
            "direct_copy: src_offset {}, dst_offset {}, len {}",
            src_offset,
            dst_offset,
            len
        );

        let mut fa = FrameAllocator::new(
            FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        while len > 0 {
            self_pt
                .with_frame(
                    src_offset as u64,
                    FindFrameFlags::empty(),
                    &mut false,
                    |src_frame_start, self_frame| {
                        if self_frame.is_some() {
                            if dst_pt.get_frame(dst_offset as u64).is_none() {
                                let frame = fa.try_allocate().ok_or(ResourceError::OutOfMemory)?;
                                let r = dst_pt.map_page(dst_offset as u64, frame);
                                if r.is_err() {
                                    fa.abort([frame]);
                                    return r;
                                }
                            }
                        }
                        dst_pt.with_frame(
                            dst_offset as u64,
                            FindFrameFlags::WRITE,
                            &mut false,
                            |dst_frame_start, dst_frame| {
                                let dst_frame_offset = dst_offset - dst_frame_start;

                                log::trace!("got frames: src_frame_start {:x}, dst_frame_start {:x}, dst_frame_offset {:x}, src_offset {:x}, dst_offset {:x}, len {}, frames = src_frame {:?}, dst_frame {:?}", src_frame_start, dst_frame_start, dst_frame_offset, src_offset, dst_offset, len, self_frame, dst_frame);
                                let dst_ptr_info = if let Some(dst_frame) = dst_frame {
                                    let dst_ptr = unsafe {
                                        dst_frame
                                            .virtaddr()
                                            .as_mut_ptr::<u8>()
                                            .byte_add(dst_frame_offset)
                                    };
                                    let dst_frame_rem = dst_frame.size() - dst_frame_offset;
                                    Some((dst_ptr, dst_frame_rem))
                                } else {
                                    None
                                };

                                let op_len = if let Some(src_frame) = self_frame {
                                    let (dst_ptr, dst_frame_rem) = dst_ptr_info.expect("destination frame should be present");
                                    let dst_frame = dst_frame.expect("destination frame should be present");
                                    let src_frame_offset = src_offset - src_frame_start;
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
                                    log::debug!(
                                        "copying {} bytes from src {:x} to dst {:x} in object {}",
                                        copy_len,
                                        src_ptr as usize,
                                        dst_ptr as usize,
                                        dst.id()
                                    );
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
                                    if let Some((dst_ptr, dst_frame_rem)) = dst_ptr_info {
                                    let zero_len = core::cmp::min(len, dst_frame_rem);
                                    log::debug!(
                                        "zeroing {} bytes at dst offset {} in object {}",
                                        zero_len,
                                        dst_offset,
                                        dst.id()
                                    );
                                    unsafe {
                                        core::ptr::write_bytes(dst_ptr, 0, zero_len);
                                    }
                                    zero_len
                                    } else {PageNumber::PAGE_SIZE.min(len)}
                                };

                                src_offset += op_len;
                                dst_offset += op_len;
                                len -= op_len;
                                Ok::<(), TwzError>(())
                            },
                        ).flatten()
                    },
                )
                .flatten()?;
        }

        // Both locks off before either one's shootdown wait runs; dropping them implicitly would
        // run the inner guard's wait under the outer lock.
        PtGuard::release_two(self_pt, dst_pt);
        Ok(())
    }

    pub fn cow_copy(
        self: &ObjectRef,
        dst: &ObjectRef,
        src_offset: usize,
        dst_offset: usize,
        len: usize,
    ) -> Result<(), TwzError> {
        if len == 0 {
            return Ok(());
        }

        log::debug!(
            "cow_copy: src_offset {:x}, dst_offset {:x}, len {} ({} => {})",
            src_offset,
            dst_offset,
            len,
            self.id(),
            dst.id()
        );
        assert!(src_offset.is_multiple_of(PHYS_LEVEL_LAYOUTS[0].size()));
        assert!(dst_offset.is_multiple_of(PHYS_LEVEL_LAYOUTS[0].size()));
        assert!(len.is_multiple_of(PHYS_LEVEL_LAYOUTS[0].size()));

        if self.use_pager() {
            let _ = self.ensure_in_core(
                self.lock_page_tables(),
                PageNumber::from_offset(src_offset),
                len / PageNumber::PAGE_SIZE,
                &mut false,
                &mut false,
            )?;
        }

        let (mut self_pt, mut dst_pt) = PtGuard::new_two(&self.tables, &dst.tables);

        let src_level = max_level_for_addr(src_offset).ok_or(TwzError::INVALID_ARGUMENT)?;
        let dst_level = max_level_for_addr(dst_offset).ok_or(TwzError::INVALID_ARGUMENT)?;
        let level = core::cmp::min(src_level, dst_level);
        self_pt.split_to_level(src_offset as u64, level)?;
        if len > PageNumber::PAGE_SIZE {
            self_pt.split_to_level((src_offset + len) as u64, level)?;
        }

        dst_pt.setup_zero_range(dst_offset as u64, len)?;
        self_pt.setup_cow_range(&mut *dst_pt, src_offset as u64, dst_offset as u64, len)?;

        // Both locks off before either one's shootdown wait runs; dropping them implicitly would
        // run the inner guard's wait under the outer lock.
        PtGuard::release_two(self_pt, dst_pt);

        if false {
            let ok = self.obj_memcmp(dst, len, src_offset, dst_offset)?;
            if !ok {
                log::error!(
                    "cow_copy: memcmp failed after copy from {} to {} (src_offset {:x}, dst_offset {:x}, len {})",
                    self.id(),
                    dst.id(),
                    src_offset,
                    dst_offset,
                    len
                );
            }
            assert!(ok);
        }

        Ok(())
    }

    pub fn copy_range(
        self: &ObjectRef,
        dst: &ObjectRef,
        src_offset: usize,
        dst_offset: usize,
        len: usize,
    ) -> Result<(), TwzError> {
        if len == 0 {
            return Ok(());
        }
        log::debug!(
            "copy_range: src_offset {:x}, dst_offset {:x}, len {} ({} => {})",
            src_offset,
            dst_offset,
            len,
            self.id(),
            dst.id()
        );
        let min_align = PHYS_LEVEL_LAYOUTS[0].size();
        let pre_copy = src_offset % min_align;
        let pre_copy_dst = dst_offset % min_align;

        if pre_copy_dst != pre_copy {
            log::trace!(
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
            assert!(
                pre_len > 0,
                "pre_len should be greater than 0, but got {} (src_offset {:x}, dst_offset {:x}, len {})",
                pre_len,
                src_offset,
                dst_offset,
                len
            );
            self.direct_copy(dst, src_offset, dst_offset, pre_len)?;
            return self.copy_range(
                dst,
                src_offset + pre_len,
                dst_offset + pre_len,
                len - pre_len,
            );
        }

        let post_copy = len % min_align;
        if post_copy != 0 {
            let post_len = post_copy;
            assert!(post_len > 0);
            self.direct_copy(
                dst,
                src_offset + len - post_len,
                dst_offset + len - post_len,
                post_len,
            )?;
            return self.copy_range(dst, src_offset, dst_offset, len - post_len);
        }

        self.cow_copy(dst, src_offset, dst_offset, len)
    }

    pub fn zero_range(self: &ObjectRef, offset: usize, len: usize) -> Result<(), TwzError> {
        log::trace!(
            "zero_range: offset {:x} len {:x} in {}",
            offset,
            len,
            self.id()
        );
        if len == 0 {
            return Ok(());
        }
        if self.use_pager() {
            log::warn!("TODO: zero_range with pager for object {}", self.id());
            return self.set_bytes(offset, len, 0);
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

        log::trace!(
            "zero_range: unmap for offset {:x} len {:x} in {}",
            offset,
            len,
            self.id()
        );
        pt.setup_zero_range(offset as u64, len)?;
        Ok(())
    }

    pub fn obj_memcmp(
        self: &ObjectRef,
        other: &ObjectRef,
        mut len: usize,
        mut self_offset: usize,
        mut other_offset: usize,
    ) -> Result<bool, TwzError> {
        if len == 0 {
            return Ok(true);
        }

        while len > 0 {
            let cmp_len = core::cmp::min(len, PHYS_LEVEL_LAYOUTS[0].size());
            let mut self_buf = [0u8; PHYS_LEVEL_LAYOUTS[0].size()];
            let mut other_buf = [0u8; PHYS_LEVEL_LAYOUTS[0].size()];

            self.read_bytes(&mut self_buf[0..cmp_len], self_offset)?;
            other.read_bytes(&mut other_buf[0..cmp_len], other_offset)?;

            if self_buf[0..cmp_len] != other_buf[0..cmp_len] {
                log::error!(
                    "obj_memcmp: memcmp failed between {} and {} (self_offset {:x}, other_offset {:x}, len {}): {:?} vs {:?}",
                    self.id(),
                    other.id(),
                    self_offset,
                    other_offset,
                    cmp_len,
                    &self_buf[0..cmp_len],
                    &other_buf[0..cmp_len]
                );
                return Ok(false);
            }

            self_offset += cmp_len;
            other_offset += cmp_len;
            len -= cmp_len;
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use twizzler_kernel_macros::kernel_test;

    #[kernel_test]
    fn test_page_number_align_down() {
        use super::PageNumber;

        // Test aligning down with power-of-2 alignments
        assert_eq!(PageNumber(15).align_down(1), PageNumber(15));
        assert_eq!(PageNumber(15).align_down(2), PageNumber(14));
        assert_eq!(PageNumber(15).align_down(4), PageNumber(12));
        assert_eq!(PageNumber(15).align_down(8), PageNumber(8));
        assert_eq!(PageNumber(15).align_down(16), PageNumber(0));

        // Test with already aligned values
        assert_eq!(PageNumber(16).align_down(16), PageNumber(16));
        assert_eq!(PageNumber(32).align_down(8), PageNumber(32));
        assert_eq!(PageNumber(64).align_down(32), PageNumber(64));

        // Test with zero
        assert_eq!(PageNumber(0).align_down(1), PageNumber(0));
        assert_eq!(PageNumber(0).align_down(4), PageNumber(0));
        assert_eq!(PageNumber(0).align_down(16), PageNumber(0));

        // Test edge cases
        assert_eq!(PageNumber(1).align_down(2), PageNumber(0));
        assert_eq!(PageNumber(7).align_down(8), PageNumber(0));
        assert_eq!(PageNumber(255).align_down(256), PageNumber(0));
    }
}
