//! Manage physical frames.
//!
//! On kernel initialization, the system will call into [init] in this module to pass information
//! about physical memory regions. Once that call completes, the physical frame allocator is ready
//! for use. This has to happen before any fully-bootstrapped memory manager is ready to use. Note,
//! though, that this module may have to perform memory allocation during initialization, so it'll
//! have to make use of the bootstrap memory allocator.
//!
//! Physical frames are physical pages of memory, whose size depends on the architecture compiled
//! for. A given physical frame can either be zeroed (that is, the physical memory the frame refers
//! to contains only zeros), or it can be indeterminate. This distinction is maintained because it's
//! common that we need to allocate zero pages AND pages that will be immediately overwritten. Upon
//! allocation, the caller can request a zeroed frame or an indeterminate frame. The allocator will
//! try to reserve known-zero frames for allocations that request them.
//!
//! Allocation returns a [FrameRef], which is a static-lifetime reference to a [Frame]. The [Frame]
//! is a bit of metadata associated with each physical frame in the system. One can efficiently get
//! the [FrameRef] given a physical address, and vice versa.
//!
//! Allocations can specify a Layout. This is a little more restrictive than standard allocations
//! in that the layout will be respected, but the physical memory allocator only really allocates
//! in architecturally-defined chunks (e.g. on x86_64, 4K, 2M, 1G). Large frames can be split into
//! smaller ones.
//!
//! Note: this code is somewhat cursed, since it needs to do a bunch of funky low-level memory
//! management without ever triggering the memory manager (can't allocate memory, since that could
//! recurse or deadlock), and we'll need the ability to store sets of pages without allocating
//! memory outside of this module as well, hence the intrusive linked list design. Additionally, the
//! kernel needs to be able to access frame data from possibly any CPU, so the whole type must be
//! both Sync and Send. This would be easy with the lock-around-inner trick, but this plays badly
//! with the intrusive list, and so we do some cursed manual locking to ensure write isolation.
//!
//! Note: This code uses intrusive linked lists (a type of intrusive data structure). These are
//! standard practice in C kernels, but are rarely needed these days. An intrusive list is a list
//! that stores the list's link data inside the nodes (`struct Foo {link: Link, ...}`) as opposed to
//! storing the objects in the list (`struct ListItem<T> {item: T, link: Link}`). They are useful
//! here because they can form arbitrary containers while ensuring no memory is allocated to store
//! the list, something that is very important inside an allocator for physical pages. For more information, see: [<https://docs.rs/intrusive-collections/latest/intrusive_collections/>].

use alloc::vec::Vec;
use core::{
    alloc::Layout,
    mem::{size_of, transmute},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use ferroc::heap;
use intrusive_collections::{LinkedList, LinkedListLink, intrusive_adapter};
use twizzler_abi::syscall::MemoryStats;

use super::{MemoryRegion, MemoryRegionKind, PhysAddr};
use crate::{
    arch::{
        VirtAddr,
        memory::{
            frame::{self, FRAME_SIZE},
            phys_to_virt,
        },
    },
    memory::tracker::{FrameAllocator, is_low_mem},
    once::Once,
    processor::sched::needs_reschedule,
    spinlock::Spinlock,
};

pub type FrameRef = &'static Frame;
type FrameMutRef = &'static mut Frame;

struct AllocationRegionLevel {
    alloc_size: usize,
    align: usize,
    free: usize,
    free_zeroed: usize,
    zeroed: LinkedList<FrameAdapter>,
    non_zeroed: LinkedList<FrameAdapter>,
}

pub const NR_LEVELS: usize = 3;

pub const PHYS_LEVEL_LAYOUTS: [Layout; NR_LEVELS] = [
    unsafe { Layout::from_size_align_unchecked(FRAME_SIZE, FRAME_SIZE) },
    unsafe { Layout::from_size_align_unchecked(FRAME_SIZE * 512, FRAME_SIZE * 512) },
    unsafe { Layout::from_size_align_unchecked(FRAME_SIZE * 512 * 512, FRAME_SIZE * 512 * 512) },
];

pub fn max_level_for_addr(addr: usize) -> Option<usize> {
    for (i, layout) in PHYS_LEVEL_LAYOUTS.iter().enumerate().rev() {
        if addr.is_multiple_of(layout.align()) && addr.is_multiple_of(layout.size()) {
            return Some(i);
        }
    }
    None
}

pub fn min_level_for_len(len: usize) -> Option<usize> {
    for (i, layout) in PHYS_LEVEL_LAYOUTS.iter().enumerate() {
        if len <= layout.size() {
            return Some(i);
        }
    }
    None
}

/// Level-0 frames per level-1 frame, i.e. how many 4 KiB frames have to be free at once before a
/// group can be coalesced back into one large frame.
const GROUP_PAGES: usize = PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size();

/// Groups rebuilt into a large frame. Worth a counter of its own: whether large frames come back
/// at all once the level-1 list has drained is the question this whole mechanism exists to answer,
/// and it is invisible from the outside -- a boot with none looks exactly like a boot without the
/// code.
static COALESCED: AtomicU64 = AtomicU64::new(0);

#[doc(hidden)]
struct AllocationRegion {
    indexer: FrameIndexer,
    nr_pages: usize,
    levels: [AllocationRegionLevel; NR_LEVELS],
    /// Free level-0 frames in each level-1-aligned group, indexed from `group_base`.
    ///
    /// Splitting is one-way without this: a large frame broken up to satisfy 4 KiB allocations
    /// never reforms, so the level-1 list drains and every later large-frame request fails --
    /// which is what makes objects assemble regions 4 KiB at a time (`promote.md`). The
    /// counter is what lets a fully-free group be *found* rather than scanned for, so the free
    /// path stays a single increment and all the work happens on the allocation that would
    /// otherwise fail.
    ///
    /// A group only partly inside this region simply never reaches `GROUP_PAGES` and is never a
    /// candidate.
    group_free: &'static mut [u16],
    group_base: PhysAddr,
}

// Safety: this is needed because of the raw pointer, but the raw pointer is static for the life of
// the kernel.
unsafe impl Send for AllocationRegion {}

impl AllocationRegionLevel {
    fn new(layout: Layout) -> Self {
        Self {
            alloc_size: layout.size(),
            align: layout.align(),
            free: 0,
            free_zeroed: 0,
            zeroed: LinkedList::new(FrameAdapter::NEW),
            non_zeroed: LinkedList::new(FrameAdapter::NEW),
        }
    }

    fn allocate_zeroable(&mut self) -> Option<FrameRef> {
        if let Some(f) = self.non_zeroed.pop_back() {
            self.free -= 1;
            return Some(f);
        }
        None
    }

    fn free(&mut self, frame: FrameRef) {
        assert!(frame.refcount() == 0);
        if frame.is_zeroed() {
            self.zeroed.push_back(frame);
            self.free_zeroed += 1;
        } else {
            self.non_zeroed.push_back(frame);
        }
        self.free += 1;
    }

    fn allocate(&mut self, try_zero: bool, only_zero: bool) -> Option<FrameRef> {
        if only_zero {
            if let Some(f) = self.zeroed.pop_back() {
                self.free -= 1;
                self.free_zeroed -= 1;
                return Some(f);
            }
            return None;
        }
        if let Some(f) = self.non_zeroed.pop_back() {
            self.free -= 1;
            return Some(f);
        }
        if try_zero {
            if let Some(f) = self.zeroed.pop_back() {
                self.free -= 1;
                self.free_zeroed -= 1;
                return Some(f);
            }
        }
        None
    }

    fn admit_one(
        &mut self,
        frame: FrameMutRef,
        addr: PhysAddr,
        level: u8,
        init_flags: PhysicalFrameFlags,
    ) -> bool {
        // `reset` force-unlinks, which would *silently* corrupt whichever list a still-linked
        // frame is on (a free list, or a deferred-unmap list awaiting shootdown). Every legitimate
        // caller hands over frames that are off every list, so a linked frame here is a bug worth
        // a panic that names it, not an unlink that defers the crash.
        assert!(
            !frame.link.is_linked(),
            "admitting frame {:?} that is still linked into a list",
            frame
        );
        // Safety: the frame can be reset since during admit_one we are the only ones with access to
        // the frame data.
        unsafe { frame.reset(addr, level, init_flags, 0) };
        frame.set_admitted();
        frame.set_free();
        assert!(frame.refcount() == 0);
        if init_flags.contains(PhysicalFrameFlags::ZEROED) {
            self.zeroed.push_back(frame);
            self.free_zeroed += 1;
        } else {
            self.non_zeroed.push_back(frame);
        }
        self.free += 1;
        true
    }
}

impl AllocationRegion {
    fn contains(&self, pa: PhysAddr) -> bool {
        self.indexer.contains(pa)
    }

    fn get_frame(&self, pa: PhysAddr) -> Option<FrameRef> {
        self.indexer.get_frame(pa)
    }

    /// Get a mutable frame reference.
    ///
    /// # Safety
    /// pa must be a new frame
    unsafe fn get_frame_mut(&mut self, pa: PhysAddr) -> Option<FrameMutRef> {
        unsafe { self.indexer.get_frame_mut(pa) }
    }

    fn free(&mut self, frame: FrameRef) {
        if !self.contains(frame.start_address()) {
            return;
        }
        assert!(frame.refcount() == 0);
        frame.set_free();
        let level = frame.get_level();
        assert!(level < NR_LEVELS);
        let addr = frame.start_address();
        self.levels[level].free(frame);
        if level == 0 {
            self.group_track(addr, 1);
        }
    }

    fn find_level(&self, layout: Layout) -> Option<usize> {
        self.levels
            .iter()
            .position(|level| level.alloc_size >= layout.size() && level.align >= layout.align())
    }

    fn group_idx(&self, pa: PhysAddr) -> Option<usize> {
        let idx =
            (pa.raw().checked_sub(self.group_base.raw())? as usize) / PHYS_LEVEL_LAYOUTS[1].size();
        (idx < self.group_free.len()).then_some(idx)
    }

    /// Record that a level-0 frame has entered (`delta > 0`) or left the level-0 free lists.
    fn group_track(&mut self, pa: PhysAddr, delta: i16) {
        if let Some(idx) = self.group_idx(pa) {
            let count = self.group_free[idx] as i16 + delta;
            assert!(count >= 0 && count as usize <= GROUP_PAGES);
            self.group_free[idx] = count as u16;
        }
    }

    /// Rebuild one fully-free group as a single level-1 frame, and hand it back.
    ///
    /// The inverse of [Self::split]. Not [Self::merge_frame], which is for *allocated* runs and
    /// asserts its children are `ALLOCATED`.
    fn coalesce_group(&mut self, idx: usize) -> Option<FrameRef> {
        let base = self
            .group_base
            .offset(idx * PHYS_LEVEL_LAYOUTS[1].size())
            .ok()?;

        // The counter says the group is whole; confirm it against the frames themselves before
        // unlinking anything, since a wrong count here would corrupt a free list. `is_linked` is
        // the precise test: an admitted, unallocated frame that is on no list is one this function
        // has already taken.
        let mut all_zeroed = true;
        for i in 0..GROUP_PAGES {
            let frame = self.indexer.get_frame(base.offset(i * FRAME_SIZE).ok()?)?;
            if frame.get_level() != 0
                || frame.refcount() != 0
                || !frame.link.is_linked()
                || frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED)
            {
                return None;
            }
            all_zeroed &= frame.is_zeroed();
        }

        for i in 0..GROUP_PAGES {
            // Unwrap-Ok: checked in the loop above.
            let frame = self
                .indexer
                .get_frame(base.offset(i * FRAME_SIZE).unwrap())
                .unwrap();
            let level = &mut self.levels[0];
            let list = if frame.is_zeroed() {
                level.free_zeroed -= 1;
                &mut level.zeroed
            } else {
                &mut level.non_zeroed
            };
            // Safety: the frame is linked into this list -- `is_linked` above, and a level-0 frame
            // is only ever on one of these two, chosen by the same ZEROED test.
            unsafe { list.cursor_mut_from_ptr(frame as *const Frame).remove() };
            level.free -= 1;

            if i > 0 {
                // Leave the children in the state `merge_frame` leaves them: no longer admitted,
                // and belonging to the frame above rather than to themselves. `split_and_keep`
                // asserts on exactly this when the large frame is taken apart again, which is what
                // the pager's donation path does to every large frame it is given.
                let pa = base.offset(i * FRAME_SIZE).unwrap();
                let child_flags = if all_zeroed {
                    PhysicalFrameFlags::ZEROED
                } else {
                    PhysicalFrameFlags::empty()
                };
                // Safety: same as `merge_frame` -- the frame has just been taken off every list and
                // is unreachable until the frame above it is split again.
                let child = unsafe { self.get_frame_mut(pa) }.unwrap();
                unsafe { child.reset(pa, 0, child_flags, 0) };
            }
        }
        self.group_free[idx] = 0;

        let flags = if all_zeroed {
            PhysicalFrameFlags::ZEROED
        } else {
            PhysicalFrameFlags::empty()
        };
        // Safety: every frame in the group is now unreachable -- unlinked and unallocated -- so the
        // head can be re-admitted at the level above.
        let head = unsafe { self.get_frame_mut(base) }?;
        self.levels[1].admit_one(head, base, 1, flags);
        let count = COALESCED.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_power_of_two() {
            log::info!("COALESCE: {} groups rebuilt into large frames", count);
        }
        self.indexer.get_frame(base)
    }

    /// Find a fully-free group and coalesce it. Called only when a large-frame request is about to
    /// fail outright, so the scan is paid for by an allocation that would have returned `None`.
    ///
    /// Keeps scanning past a candidate `coalesce_group` rejects: the counter and the frames
    /// disagreeing is precisely the case that validation exists for, and stopping at the first one
    /// would leave that index selected forever, killing coalescing for the rest of the boot.
    fn coalesce_any(&mut self) -> Option<FrameRef> {
        let mut next = 0;
        while let Some(off) = self.group_free[next..]
            .iter()
            .position(|count| *count as usize == GROUP_PAGES)
        {
            let idx = next + off;
            if let Some(frame) = self.coalesce_group(idx) {
                return Some(frame);
            }
            log::warn!("COALESCE: group {} counted free but was rejected", idx);
            next = idx + 1;
        }
        None
    }

    fn collect_zeroable(&mut self, frames: &mut heapless::Vec<FrameRef, 16>) {
        const MAX_BACKGROUND_ZERO_BYTES: usize = PHYS_LEVEL_LAYOUTS[1].size() * 2;
        let mut total_bytes = 0;
        for level in 0..2 {
            while total_bytes + PHYS_LEVEL_LAYOUTS[level].size() <= MAX_BACKGROUND_ZERO_BYTES
                && !frames.is_full()
                && self.levels[level].free > 0
                && self.levels[level].free_zeroed < self.levels[level].free / 2
            {
                if let Some(frame) = self.levels[level].allocate_zeroable() {
                    total_bytes += frame.size();

                    if level == 0 {
                        self.group_track(frame.start_address(), -1);
                    }
                    frames.push(frame).unwrap();
                } else {
                    break;
                }
            }
        }
    }

    fn do_allocate(&mut self, try_zero: bool, only_zero: bool, level: usize) -> Option<FrameRef> {
        if level >= NR_LEVELS {
            return None;
        }
        if let Some(frame) = self.levels[level].allocate(try_zero, only_zero) {
            if level == 0 {
                self.group_track(frame.start_address(), -1);
            }
            return Some(frame);
        }

        let bigger_frame = self.do_allocate(try_zero, only_zero, level + 1)?;
        self.split(bigger_frame);
        let frame = self.levels[level].allocate(try_zero, only_zero)?;
        if level == 0 {
            self.group_track(frame.start_address(), -1);
        }
        Some(frame)
    }

    fn allocate(&mut self, try_zero: bool, only_zero: bool, layout: Layout) -> Option<FrameRef> {
        let level = self.find_level(layout)?;
        let frame = match self.do_allocate(try_zero, only_zero, level) {
            Some(frame) => frame,
            // Nothing at this level and nothing bigger to split. Before giving up on a large-frame
            // request, rebuild one from a group that has become entirely free -- this is the point
            // where the one-way split would otherwise become permanent.
            //
            // Gated on the level-1 list being genuinely empty, not just on `do_allocate` failing:
            // it also fails when frames are there but on the wrong side of the zeroed/non-zeroed
            // split for this `try_zero`/`only_zero` pair, and rebuilding then would spend a whole
            // free group to produce what the next pass would have found anyway.
            None if level == 1 && self.levels[1].free == 0 => {
                // `coalesce_any` admits the rebuilt frame to the level-1 list rather than handing
                // it over directly, so take it back out through the normal path -- which is also
                // what applies `only_zero` to it.
                self.coalesce_any()?;
                self.levels[1].allocate(try_zero, only_zero)?
            }
            None => return None,
        };
        assert!(!frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
        frame.set_allocated();
        Some(frame)
    }

    pub fn merge_frame(&mut self, frame: FrameRef) -> FrameRef {
        if !self.contains(frame.start_address()) {
            panic!("tried to split a frame within the wrong region");
        }
        let level = frame.get_level();
        assert!(level + 1 < NR_LEVELS);

        let start = frame.start_address();
        let child_size = frame.size();
        let new_frame_size = PHYS_LEVEL_LAYOUTS[level + 1].size();
        let child_count = new_frame_size / child_size;
        // The run has to be the whole of the frame above it, not just its length: merging a
        // misaligned run yields a large frame whose children straddle two of them, which no mapping
        // can use and which puts `group_free` out of step with the frames it counts.
        assert!(start.is_aligned_to(new_frame_size));
        // Every child is ALLOCATED (asserted below), so none can be on a free list.
        if level == 0
            && let Some(idx) = self.group_idx(start)
        {
            assert_eq!(self.group_free[idx], 0);
        }

        // ZEROED describes a whole frame, so the merged frame may only claim it if every child
        // does. Taking it from the head alone would mark the full run clean on the strength of its
        // first page, and `alloc` skips `zero()` on the strength of the flag -- handing out a
        // "fresh" large frame still holding its previous tenant's data.
        // skip the first one for now, as that's our passed in frame.
        let mut all_zeroed = frame.is_zeroed();
        let rc = frame.refcount();
        for child_idx in 1..child_count {
            let pa = start.offset(child_idx * child_size).unwrap();
            let child = self.get_frame(pa).unwrap();
            assert!(child.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
            assert!(child.get_flags().contains(PhysicalFrameFlags::ADMITTED));
            // The caller claims every child; a child on a list (free, or deferred-unmap pending
            // shootdown) or at a different refcount than the head belongs to someone else, and
            // resetting it would silently corrupt that list or leak that reference.
            assert!(
                !child.link.is_linked(),
                "merging child {:?} that is still linked into a list",
                child
            );
            assert_eq!(
                child.refcount(),
                rc,
                "merging child {:?} whose refcount differs from head {:?}",
                child,
                frame
            );
            all_zeroed &= child.is_zeroed();
        }
        let merged_flags = if all_zeroed {
            PhysicalFrameFlags::ZEROED
        } else {
            PhysicalFrameFlags::empty()
        };

        for child_idx in 1..child_count {
            let pa = start.offset(child_idx * child_size).unwrap();
            let child = unsafe { self.get_frame_mut(pa) }.unwrap();
            unsafe { child.reset(pa, level as u8, merged_flags, 0) };
        }
        let rc = frame.refcount();
        let frame = unsafe { self.get_frame_mut(start) }.unwrap();
        assert!(frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
        assert!(frame.get_flags().contains(PhysicalFrameFlags::ADMITTED));
        unsafe { frame.reset(start, (level + 1) as u8, merged_flags, rc) };
        frame.set_admitted();
        frame.set_allocated();
        frame
    }

    pub fn split_and_keep(&mut self, frame: FrameRef) -> (FrameRef, usize) {
        if !self.contains(frame.start_address()) {
            panic!("tried to split a frame within the wrong region");
        }
        let level = frame.get_level();
        if level == 0 {
            return (frame, PHYS_LEVEL_LAYOUTS[0].size());
        }

        let new_frame_size = PHYS_LEVEL_LAYOUTS[level - 1].size();
        let child_count = frame.size() / new_frame_size;
        // skip the first one for now, as that's our passed in frame.
        for child_idx in 1..child_count {
            let pa = frame
                .start_address()
                .offset(child_idx * new_frame_size)
                .unwrap();
            let child = unsafe { self.get_frame_mut(pa) }.unwrap();
            assert!(!child.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
            assert!(!child.get_flags().contains(PhysicalFrameFlags::ADMITTED));
            unsafe {
                child.reset(
                    pa,
                    (level - 1) as u8,
                    frame.get_flags() & PhysicalFrameFlags::ZEROED,
                    frame.refcount(),
                )
            };
            child.set_admitted();
            child.set_allocated();
        }
        let frame = unsafe { self.get_frame_mut(frame.start_address()) }.unwrap();
        assert!(frame.get_flags().contains(PhysicalFrameFlags::ADMITTED));
        assert!(frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
        unsafe {
            frame.reset(
                frame.start_address(),
                (level - 1) as u8,
                frame.get_flags() & PhysicalFrameFlags::ZEROED,
                frame.refcount(),
            )
        };
        frame.set_admitted();
        frame.set_allocated();
        (frame, PHYS_LEVEL_LAYOUTS[level].size())
    }

    fn split(&mut self, frame: FrameRef) {
        if !self.contains(frame.start_address()) {
            logln!("warn -- tried to split a frame within the wrong region");
            return;
        }
        let level = frame.get_level();
        assert!(level > 0);
        assert!(frame.refcount() == 0);

        let new_frame_size = PHYS_LEVEL_LAYOUTS[level - 1].size();
        let child_count = frame.size() / new_frame_size;
        // skip the first one for now, as that's our passed in frame.
        for child_idx in 1..child_count {
            let pa = frame
                .start_address()
                .offset(child_idx * new_frame_size)
                .unwrap();
            let child = unsafe { self.get_frame_mut(pa) }.unwrap();
            self.levels[level - 1].admit_one(
                child,
                pa,
                (level - 1) as u8,
                frame.get_flags() & PhysicalFrameFlags::ZEROED,
            );
            if level == 1 {
                self.group_track(pa, 1);
            }
        }
        let start = frame.start_address();
        let frame = unsafe { self.get_frame_mut(start) }.unwrap();
        self.levels[level - 1].admit_one(
            frame,
            start,
            (level - 1) as u8,
            frame.get_flags() & PhysicalFrameFlags::ZEROED,
        );
        if level == 1 {
            self.group_track(start, 1);
        }
    }

    fn new(m: &MemoryRegion) -> Option<Self> {
        let start = m.start.align_up(FRAME_SIZE as u64).unwrap();
        let length = m.length - (start.raw() - m.start.raw()) as usize;
        let nr_pages = length / FRAME_SIZE;
        if nr_pages <= 1 {
            return None;
        }
        let frame_array_len = size_of::<Frame>() * nr_pages;
        // The group counters live in the region's own reserved pages, right behind the frame array,
        // because this runs before there is a heap worth allocating from.
        // Over-approximated by two: the first group can start below this region, since it is keyed
        // to a level-1-aligned address rather than to the region's own start.
        let nr_groups = nr_pages.div_ceil(GROUP_PAGES) + 2;
        let group_array_len = size_of::<u16>() * nr_groups;
        let array_pages = ((frame_array_len + group_array_len - 1) / FRAME_SIZE) + 1;
        if array_pages >= nr_pages {
            return None;
        }

        let frame_array_ptr: *mut Frame = phys_to_virt(start).as_mut_ptr();
        // Safety: the reservation above covers both arrays, and `Frame` is 8-aligned so the frame
        // array's length keeps the group array aligned for u16.
        let group_free = unsafe {
            let ptr = frame_array_ptr.byte_add(frame_array_len) as *mut u16;
            core::slice::from_raw_parts_mut(ptr, nr_groups)
        };
        group_free.fill(0);

        let mut levels = [
            AllocationRegionLevel::new(PHYS_LEVEL_LAYOUTS[0]),
            AllocationRegionLevel::new(PHYS_LEVEL_LAYOUTS[1]),
            AllocationRegionLevel::new(PHYS_LEVEL_LAYOUTS[2]),
        ];

        // Safety: the pointer is to a static region of reserved memory.
        let mut indexer = unsafe {
            FrameIndexer::new(
                start.offset(array_pages * FRAME_SIZE).unwrap(),
                (nr_pages - array_pages) * FRAME_SIZE,
                frame_array_ptr,
                nr_pages,
            )
        };

        // Organize into levels.
        let mut cursor = start.offset(array_pages * FRAME_SIZE).unwrap();
        let end = start.offset(nr_pages * FRAME_SIZE).unwrap();
        // Unwrap-Ok: aligning down cannot leave the canonical range.
        let group_base = cursor
            .align_down(PHYS_LEVEL_LAYOUTS[1].size() as u64)
            .unwrap();
        while cursor < end {
            let remaining = end - cursor;
            // select level based on alignment and space
            // Unwrap-Ok: level 0 will always work.
            let level = (NR_LEVELS - 1)
                - levels
                    .iter()
                    .rev()
                    .position(|level| {
                        cursor.is_aligned_to(level.align) && remaining >= level.alloc_size
                    })
                    .unwrap();
            // Unwrap-Ok: we know this address is in this region already
            // Safety: we are allocating a new, untouched frame here
            let frame = unsafe { indexer.get_frame_mut(cursor) }.unwrap();
            for i in 0..PHYS_LEVEL_LAYOUTS[level].size() / PHYS_LEVEL_LAYOUTS[0].size() {
                let sub_addr = cursor.offset(i * PHYS_LEVEL_LAYOUTS[0].size()).unwrap();
                unsafe {
                    indexer.get_frame_mut(sub_addr).unwrap().reset(
                        sub_addr,
                        0,
                        PhysicalFrameFlags::empty(),
                        0,
                    )
                };
            }
            levels[level].admit_one(frame, cursor, level as u8, PhysicalFrameFlags::empty());
            if level == 0 {
                let idx = (cursor - group_base) as usize / PHYS_LEVEL_LAYOUTS[1].size();
                group_free[idx] += 1;
            }
            cursor = cursor.offset(levels[level].alloc_size).unwrap();
        }

        Some(Self {
            indexer,
            levels,
            nr_pages,
            group_free,
            group_base,
        })
    }
}

#[doc(hidden)]
struct PhysicalFrameAllocator {
    regions: Vec<AllocationRegion>,
    admitted_regions: Vec<(PhysAddr, usize)>,
    region_idx: usize,
}

/// A physical frame.
///
/// Contains a physical address and flags that indicate if the frame is zeroed or not.
pub struct Frame {
    pa: PhysAddr,
    info: AtomicU64,
    link: LinkedListLink,
}
intrusive_adapter!(pub FrameAdapter = &'static Frame: Frame { link: LinkedListLink });

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl core::fmt::Debug for Frame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Frame")
            .field("pa", &self.pa)
            .field("flags", &self.get_flags())
            .field("level", &self.get_level())
            .finish()
    }
}

impl Frame {
    // Safety: must only be called once, during admit_one, when the frame has not been initialized
    // yet.
    #[track_caller]
    unsafe fn reset(&mut self, pa: PhysAddr, level: u8, init_flags: PhysicalFrameFlags, rc: u32) {
        if rc > 0 {
            log::debug!(
                "admitting frame with non-zero refcount: {}: {}",
                rc,
                core::panic::Location::caller()
            );
        }
        self.info.store(
            (init_flags.bits() as u64) | ((level as u64) << 8) | ((rc as u64) << 32),
            Ordering::SeqCst,
        );
        let pa_ptr = &mut self.pa as *mut _;
        unsafe {
            *pa_ptr = pa;
            self.link.force_unlink();
        }
        // This store acts as a release for pa as well, which synchronizes with a load in lock (or
        // unlock), which is always called at least once during allocation, so any thread
        // that accesses a frame syncs-with this write.
        self.unlock();
    }

    fn lock(&self) {
        while self
            .info
            .fetch_or(PhysicalFrameFlags::LOCKED.bits() as u64, Ordering::SeqCst)
            & PhysicalFrameFlags::LOCKED.bits() as u64
            != 0
        {
            crate::arch::processor::spin_wait_iteration();
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.info.fetch_and(
            !(PhysicalFrameFlags::LOCKED.bits() as u64),
            Ordering::SeqCst,
        );
    }

    fn reset_refcount(&self) {
        self.info.fetch_and(!(0xFFFFFFFF << 32), Ordering::SeqCst);
    }

    /// Get the start address of the frame.
    pub fn start_address(&self) -> PhysAddr {
        self.pa
    }

    fn get_level(&self) -> usize {
        ((self.info.load(Ordering::SeqCst) >> 8) & 0xFF) as usize
    }

    /// Get the length of the frame in bytes.
    pub fn size(&self) -> usize {
        PHYS_LEVEL_LAYOUTS[self.get_level()].size()
    }

    pub fn nr_pages(&self) -> usize {
        self.size() / PHYS_LEVEL_LAYOUTS[0].size()
    }

    pub fn refcount(&self) -> u32 {
        (self.info.load(Ordering::SeqCst) >> 32) as u32
    }

    pub fn inc_refcount(&self) {
        self.info.fetch_add(1 << 32, Ordering::SeqCst);
    }

    pub fn dec_refcount(&self) -> u32 {
        assert!(self.refcount() > 0);
        (self.info.fetch_sub(1 << 32, Ordering::SeqCst) >> 32) as u32 - 1
    }

    pub fn is_pt(&self) -> bool {
        self.get_flags().contains(PhysicalFrameFlags::IS_PT)
    }

    pub fn set_pt(&self, is_pt: bool) -> bool {
        self.set_flags(PhysicalFrameFlags::IS_PT, is_pt)
            .contains(PhysicalFrameFlags::IS_PT)
    }

    pub fn is_cow(&self) -> bool {
        self.get_flags().contains(PhysicalFrameFlags::IS_COW)
    }

    pub fn set_cow(&self, is_cow: bool) -> bool {
        self.set_flags(PhysicalFrameFlags::IS_COW, is_cow)
            .contains(PhysicalFrameFlags::IS_COW)
    }

    /// Zero a frame.
    ///
    /// This marks a frame as being zeroed and also set the underlying physical memory to zero.
    pub fn zero(&self) {
        self.lock();
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };
        slice.fill(0);
        self.set_flags(PhysicalFrameFlags::ZEROED, true);
        // The contents are no longer the poison pattern; see `FREE_POISON`.
        self.info.fetch_and(!POISON_BIT, Ordering::SeqCst);
        self.unlock();
    }

    fn set_poisoned(&self) {
        self.info.fetch_or(POISON_BIT, Ordering::SeqCst);
    }

    fn take_poisoned(&self) -> bool {
        self.info.fetch_and(!POISON_BIT, Ordering::SeqCst) & POISON_BIT != 0
    }

    /// Mark this frame as not being zeroed. Does not modify the physical memory controlled by this
    /// Frame.
    pub fn set_not_zero(&self) {
        self.lock();
        self.set_flags(PhysicalFrameFlags::ZEROED, false);
        self.unlock();
    }

    /// Check if this frame is marked as zeroed. Does not look at the underlying physical memory.
    pub fn is_zeroed(&self) -> bool {
        self.get_flags().contains(PhysicalFrameFlags::ZEROED)
    }

    fn set_admitted(&self) {
        self.set_flags(PhysicalFrameFlags::ADMITTED, true);
    }

    fn set_free(&self) {
        self.set_flags(PhysicalFrameFlags::ALLOCATED, false);
    }

    fn set_allocated(&self) {
        self.set_flags(PhysicalFrameFlags::ALLOCATED, true);
    }

    pub fn set_kernel(&self, kernel: bool) {
        self.set_flags(PhysicalFrameFlags::KERNEL, kernel);
    }

    pub fn is_kernel(&self) -> bool {
        self.get_flags().contains(PhysicalFrameFlags::KERNEL)
    }

    pub fn set_wired(&self, wired: bool) {
        self.set_flags(PhysicalFrameFlags::IS_WIRED, wired);
    }

    pub fn is_wired(&self) -> bool {
        self.get_flags().contains(PhysicalFrameFlags::IS_WIRED)
    }

    /// Get the current flags.
    pub fn get_flags(&self) -> PhysicalFrameFlags {
        PhysicalFrameFlags::from_bits_truncate(self.info.load(Ordering::SeqCst) as u8)
    }

    pub fn set_flags(&self, flags: PhysicalFrameFlags, set: bool) -> PhysicalFrameFlags {
        let old = if set {
            self.info.fetch_or(flags.bits() as u64, Ordering::SeqCst)
        } else {
            self.info
                .fetch_and(!(flags.bits() as u64), Ordering::SeqCst)
        };
        PhysicalFrameFlags::from_bits_truncate(old as u8)
    }

    pub fn virtaddr(&'static self) -> VirtAddr {
        phys_to_virt(self.pa)
    }

    pub fn as_slice<T>(&'static self) -> &'static [T] {
        let virt = phys_to_virt(self.pa);
        let ptr: *const T = virt.as_ptr();
        let len = self.size() / core::mem::size_of::<T>();
        unsafe { core::slice::from_raw_parts(ptr, len) }
    }

    pub fn as_byte_slice(&'static self) -> &'static [u8] {
        let virt = phys_to_virt(self.pa);
        let ptr: *const u8 = virt.as_ptr();
        unsafe { core::slice::from_raw_parts(ptr, self.size()) }
    }

    pub unsafe fn as_byte_slice_mut(&'static self) -> &'static mut [u8] {
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) }
    }

    /// Copy contents of one frame into another. If the other frame is marked as zeroed, copying
    /// will not happen. Both frames are locked first.
    pub fn copy_contents_from(&self, other: &Frame, doff: usize, soff: usize, len: usize) {
        self.lock();
        // We don't need to lock the other frame, since if its contents aren't synchronized with
        // this operation, it could have reordered to before or after.
        if other.is_zeroed() {
            // if both are zero, do nothing
            if self.is_zeroed() {
                self.unlock();
                return;
            }
            // if other is zero and we aren't, just zero instead of copy
            let virt = phys_to_virt(self.pa);
            let ptr: *mut u8 = virt.as_mut_ptr();
            let slice = unsafe { core::slice::from_raw_parts_mut(ptr.add(doff), len) };
            slice.fill(0);
            // ZEROED describes the whole frame, so only a whole-frame zero may claim it. Setting
            // it after zeroing a sub-range leaves stale bytes outside that range while the
            // allocator's `alloc` skips `zero()` on the strength of the flag, handing out a
            // "fresh" frame still holding its previous tenant's data.
            if doff == 0 && len == self.size() {
                self.set_flags(PhysicalFrameFlags::ZEROED, true);
            }
            self.unlock();
            return;
        }

        self.set_flags(PhysicalFrameFlags::ZEROED, false);
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr.add(doff), len) };

        let othervirt = phys_to_virt(other.pa);
        let otherptr: *mut u8 = othervirt.as_mut_ptr();
        let otherslice = unsafe { core::slice::from_raw_parts_mut(otherptr.add(soff), len) };

        slice.copy_from_slice(otherslice);
        self.unlock();
    }

    /// Copy from another physical address into this frame.
    pub fn copy_contents_from_physaddr(&self, doff: usize, other: PhysAddr, len: usize) {
        self.lock();
        self.set_flags(PhysicalFrameFlags::ZEROED, false);
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr.add(doff), len) };

        let othervirt = phys_to_virt(other);
        let otherptr: *mut u8 = othervirt.as_mut_ptr();
        let otherslice = unsafe { core::slice::from_raw_parts_mut(otherptr, len) };

        slice.copy_from_slice(otherslice);
        self.unlock();
    }

    pub fn cow_frame(&'static self, alloc: &mut FrameAllocator) -> Option<FrameRef> {
        let info = self.info.load(Ordering::SeqCst);
        let flags = PhysicalFrameFlags::from_bits_truncate(info as u8);
        let refcount = (info >> 32) as u32;

        if !flags.contains(PhysicalFrameFlags::IS_COW) {
            return Some(self);
        }

        // See if we can just clear the COW flag.
        if refcount <= 1 {
            let new = info & !(PhysicalFrameFlags::IS_COW.bits() as u64);
            // TODO: should we try this in a loop a few times?
            if self
                .info
                .compare_exchange(info, new, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Some(self);
            }
        }

        let new_frame = alloc.try_allocate()?;
        new_frame.copy_contents_from(self, 0, 0, self.size());

        let new = info - (1 << 32);
        if self
            .info
            .compare_exchange(info, new, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            alloc.abort([new_frame]);
            // TODO: pause for a bit?
            return self.cow_frame(alloc);
        }
        new_frame.inc_refcount();
        if self.is_pt() {
            new_frame.set_pt(true);
        }

        Some(new_frame)
    }
}

bitflags::bitflags! {
    /// Flags to control the state of a physical frame. Also used by the alloc functions to indicate
    /// what kind of physical frame is being requested.
    #[derive(Clone, Copy, Debug)]
    pub struct PhysicalFrameFlags: u8 {
        /// The frame is zeroed (or, allocate a zeroed frame)
        const ZEROED = (1 << 0);
        /// The frame has been allocated by the system.
        const ALLOCATED = (1 << 1);
        /// (internal) The frame has been admitted into the frame tracking system.
        const ADMITTED = (1 << 2);
        /// (internal) The frame is owned by the kernel.
        const KERNEL = (1 << 3);
        const IS_PT = (1 << 4);
        const IS_COW = (1 << 5);
        const IS_WIRED = (1 << 6);

        const LOCKED = (1 << 7);
    }
}

impl PhysicalFrameAllocator {
    fn new(memory_regions: &[MemoryRegion]) -> PhysicalFrameAllocator {
        Self {
            region_idx: 0,
            admitted_regions: Vec::new(),
            regions: memory_regions
                .iter()
                .filter_map(|m| {
                    if m.kind == MemoryRegionKind::UsableRam {
                        AllocationRegion::new(m)
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }

    fn total(&self) -> usize {
        self.regions
            .iter()
            .fold(0, |acc, region| region.nr_pages + acc)
    }

    /// Take a frame off the free lists. Zeroing, if the caller asked for it, is deliberately *not*
    /// done here -- see [`raw_alloc_frame`].
    fn alloc(&mut self, flags: PhysicalFrameFlags, layout: Layout) -> Option<FrameRef> {
        let frame = self.__do_alloc(flags, layout)?;
        assert!(frame.refcount() == 0);
        Some(frame)
    }

    fn __do_alloc(&mut self, flags: PhysicalFrameFlags, layout: Layout) -> Option<FrameRef> {
        let needs_zero = flags.contains(PhysicalFrameFlags::ZEROED);
        for reg in &mut self.regions {
            let frame = reg.allocate(false, needs_zero, layout);
            if frame.is_some() {
                return frame;
            }
        }
        for reg in &mut self.regions {
            let frame = reg.allocate(true, false, layout);
            if frame.is_some() {
                return frame;
            }
        }
        None
    }

    fn free(&mut self, frame: FrameRef) {
        for reg in &mut self.regions {
            if reg.contains(frame.start_address()) {
                reg.free(frame);
                return;
            }
        }
    }

    fn split_frame(&mut self, frame: FrameRef) -> (FrameRef, usize) {
        for reg in &mut self.regions {
            if reg.contains(frame.start_address()) {
                return reg.split_and_keep(frame);
            }
        }
        panic!("could not find frame region for {:?}", frame);
    }

    fn merge_frame(&mut self, frame: FrameRef) -> FrameRef {
        for reg in &mut self.regions {
            if reg.contains(frame.start_address()) {
                return reg.merge_frame(frame);
            }
        }
        panic!("could not find frame region for {:?}", frame);
    }
}

#[doc(hidden)]
static PFA: Once<Spinlock<PhysicalFrameAllocator>> = Once::new();

#[derive(Clone)]
struct FrameIndexer {
    start: PhysAddr,
    len: usize,
    frame_array_ptr: *const Frame,
    frame_array_len: usize,
}

impl FrameIndexer {
    /// Build a new frame indexer.
    ///
    /// `frame_array_len` is a count of [Frame]s, not bytes.
    ///
    /// # Safety: The passed pointer and len must point to a valid section of memory reserved for the frame slice, which will last the lifetime of the kernel.
    unsafe fn new(
        start: PhysAddr,
        len: usize,
        frame_array_ptr: *const Frame,
        frame_array_len: usize,
    ) -> Self {
        Self {
            start,
            len,
            frame_array_ptr,
            frame_array_len,
        }
    }

    fn frame_array(&self) -> &[Frame] {
        unsafe { core::slice::from_raw_parts(self.frame_array_ptr, self.frame_array_len) }
    }

    fn frame_array_mut(&mut self) -> &mut [Frame] {
        unsafe {
            core::slice::from_raw_parts_mut(self.frame_array_ptr as *mut _, self.frame_array_len)
        }
    }

    fn get_frame(&self, pa: PhysAddr) -> Option<FrameRef> {
        if !self.contains(pa) {
            return None;
        }
        let index = (pa - self.start) / FRAME_SIZE;
        assert!(index < self.frame_array_len);
        let frame = &self.frame_array()[index as usize];
        // Safety: the frame array is static for the life of the kernel
        Some(unsafe { transmute(frame) })
    }

    unsafe fn get_frame_mut(&mut self, pa: PhysAddr) -> Option<FrameMutRef> {
        if !self.contains(pa) {
            return None;
        }
        let index = (pa - self.start) / FRAME_SIZE;
        assert!(index < self.frame_array_len);
        let frame = &mut self.frame_array_mut()[index as usize];
        // Safety: the frame array is static for the life of the kernel
        Some(unsafe { transmute(frame) })
    }

    fn contains(&self, pa: PhysAddr) -> bool {
        pa >= self.start && pa < (self.start.offset(self.len).unwrap())
    }
}

// Safety: this is needed because of the raw pointer, but the raw pointer is static for the life of
// the kernel.
unsafe impl Send for FrameIndexer {}
unsafe impl Sync for FrameIndexer {}

#[doc(hidden)]
static FI: Once<Vec<FrameIndexer>> = Once::new();

/// Initialize the global physical frame allocator.
/// # Arguments
///  * `regions`: An array of memory regions passed from the boot info system.
pub fn init(regions: &[MemoryRegion]) {
    let pfa = PhysicalFrameAllocator::new(regions);
    let total = pfa.total();
    log::info!(
        "tracking {} GB of physical memory",
        total * PHYS_LEVEL_LAYOUTS[0].size() / (1024 * 1024 * 1024)
    );
    FI.call_once(|| pfa.regions.iter().map(|r| r.indexer.clone()).collect());
    PFA.call_once(|| Spinlock::new(pfa));
    crate::memory::tracker::init(total, total, 0);
}

/// Frames taken under one acquisition of the allocator lock. Bounded so the batch lives in a
/// `heapless::Vec`: growing a heap `Vec` inside that lock would reenter the allocator through
/// `allocate_chunk` and deadlock on a spinlock that is not reentrant.
const MAX_BULK_ALLOC: usize = 32;

/// Allocate up to `count` frames, taking the allocator lock once per [`MAX_BULK_ALLOC`] rather
/// than once per frame. Returns how many were appended to `out`.
///
/// The lock is the point. At smp4 a bench boot takes it ~3M times, once per frame, and the
/// per-frame cost measured 1.3-2.8 us against ~300-650 ns of that being the zeroing this still
/// does per frame -- so the remaining three quarters is the acquisition and the free-list walk.
///
/// `out` must have capacity for `count` already: pushing is done outside the lock, but a caller
/// that lets it grow mid-loop pays a heap allocation per batch for no reason.
pub(super) fn raw_alloc_frames(
    flags: PhysicalFrameFlags,
    layout: Layout,
    count: usize,
    out: &mut alloc::vec::Vec<FrameRef>,
) -> usize {
    let mut total = 0;
    while total < count {
        let want = (count - total).min(MAX_BULK_ALLOC);
        let mut batch = heapless::Vec::<FrameRef, MAX_BULK_ALLOC>::new();
        {
            let mut pfa = PFA.wait().lock();
            for _ in 0..want {
                let Some(frame) = pfa.alloc(flags, layout) else {
                    break;
                };
                // Cannot fail: `want <= MAX_BULK_ALLOC` is the vec's capacity.
                let _ = batch.push(frame);
            }
        }
        if batch.is_empty() {
            break;
        }
        // Zeroing outside the lock, for the same reason `raw_alloc_frame` does it there.
        for frame in batch {
            finish_raw_alloc(frame, flags);
            out.push(frame);
            total += 1;
        }
    }
    total
}

/// Cross-level overlap detector. The per-hand-out `!ALLOCATED` assert cannot see a frame handed
/// out at *two different levels* -- a level-0 frame inside a still-admitted level-1 frame is a
/// distinct `Frame` struct with its own clean flags. That shape zeroes someone else's live memory
/// on the very next ZEROED allocation, and the crash it produces (a ret popping 0 off a
/// heap-backed kernel stack) points nowhere near the allocator. Check at hand-out and free
/// instead, where both frames can still be named.
///
/// Invariants checked, both directions:
/// - a frame's range must not lie inside a larger frame that is still ADMITTED (a free-listed or
///   allocated large frame owns its whole range; its children are non-admitted by `reset`);
/// - a large frame's children must all be non-admitted.
///
/// Runs without the allocator lock, which is safe against transients: `split` demotes the head to
/// level 0 before its children can be handed out, and `coalesce_group`/`merge_frame` only build a
/// large frame out of a group with no independently-live members -- so any hit is a real overlap,
/// not a mid-transition read.
const OVERLAP_CHECK: bool = true;

/// Write-after-free detector. `check_overlap` sees a frame handed out at two levels, but not the
/// other way corruption enters: a stale free of a frame whose current owner holds it at refcount
/// zero (a precharge pool, a not-yet-mapped object page) passes every assert on the free path,
/// puts the frame on the free list with two owners, and the second owner's ZEROED request memsets
/// the first owner's memory. So: fill every level-0 non-zeroed frame with a pattern as it enters
/// the free list, and verify the pattern (or, for zeroed-list frames, the zeroes) at the next
/// hand-out -- any write in between panics at hand-out, naming the frame, instead of surfacing as
/// a wild jump much later.
///
/// The bit tracking "this frame holds the pattern" lives in `info` above the flags byte; `reset`
/// clears it wholesale, and `zero()` clears it when it rewrites the contents, so split, merge,
/// coalesce, and the background zeroer only lose coverage, never false-positive.
///
/// Costs a 4 KiB write per free and a 4 KiB read per alloc: a diagnostic, not a shipping default.
/// (A poison-armed sweep also runs slow enough to trip the 25s sleep diagnostics and the bench
/// watchdog under mass frees -- `fa-poison` round 1.)
const FREE_POISON: bool = false;

const POISON_BIT: u64 = 1 << 16;
const POISON_PATTERN: u64 = 0xF4EE_F4EE_F4EE_F4EE;

fn poison_on_free(frame: FrameRef) {
    if !FREE_POISON || frame.get_level() != 0 || frame.is_zeroed() {
        return;
    }
    let ptr: *mut u64 = frame.virtaddr().as_mut_ptr();
    let words = frame.size() / size_of::<u64>();
    unsafe { core::slice::from_raw_parts_mut(ptr, words) }.fill(POISON_PATTERN);
    frame.set_poisoned();
}

fn check_poison_on_alloc(frame: FrameRef) {
    if !FREE_POISON {
        return;
    }
    let expect = if frame.take_poisoned() {
        POISON_PATTERN
    } else if frame.is_zeroed() && frame.get_level() == 0 {
        0
    } else {
        return;
    };
    let ptr: *const u64 = frame.virtaddr().as_ptr();
    let words = frame.size() / size_of::<u64>();
    let slice = unsafe { core::slice::from_raw_parts(ptr, words) };
    if let Some(idx) = slice.iter().position(|w| *w != expect) {
        panic!(
            "frame {:?} was written while on the free list: offset {:x} holds {:x}, expected {:x}",
            frame,
            idx * size_of::<u64>(),
            slice[idx],
            expect
        );
    }
}

fn check_overlap(frame: FrameRef, whence: &str) {
    if !OVERLAP_CHECK {
        return;
    }
    let pa = frame.start_address();
    let level = frame.get_level();
    for l in (level + 1)..NR_LEVELS {
        let head_pa = pa.align_down(PHYS_LEVEL_LAYOUTS[l].size() as u64).unwrap();
        if head_pa == pa {
            continue;
        }
        if let Some(head) = get_frame(head_pa)
            && head.get_flags().contains(PhysicalFrameFlags::ADMITTED)
            && head.get_level() >= l
        {
            panic!(
                "physical frame overlap ({}): {:?} lies inside admitted {:?}",
                whence, frame, head
            );
        }
    }
    if level > 0 {
        let child_size = PHYS_LEVEL_LAYOUTS[0].size();
        for i in 1..(frame.size() / child_size) {
            let cpa = pa.offset(i * child_size).unwrap();
            if let Some(child) = get_frame(cpa)
                && child.get_flags().contains(PhysicalFrameFlags::ADMITTED)
            {
                panic!(
                    "physical frame overlap ({}): admitted child {:?} inside {:?}",
                    whence, child, frame
                );
            }
        }
    }
}

/// The post-allocation half of [`raw_alloc_frame`], shared with [`raw_alloc_frames`].
fn finish_raw_alloc(frame: FrameRef, flags: PhysicalFrameFlags) {
    check_overlap(frame, "alloc");
    check_poison_on_alloc(frame);
    if flags.contains(PhysicalFrameFlags::ZEROED) && !frame.is_zeroed() {
        use crate::memory::tracker::allocprofile;
        let t = allocprofile::start();
        frame.zero();
        allocprofile::add(&allocprofile::ZEROED_INLINE, 1);
        allocprofile::record(&allocprofile::ZERO_NS, t);
    }
    if flags.contains(PhysicalFrameFlags::ZEROED) {
        assert!(frame.is_zeroed());
    }
    /* TODO: try to use the MMU to detect if a page is actually ever written to or not */
    frame.set_not_zero();
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ADMITTED));
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
}

pub(super) fn raw_alloc_frame(flags: PhysicalFrameFlags, layout: Layout) -> Option<FrameRef> {
    let frame = { PFA.wait().lock().alloc(flags, layout) }?;
    // Zeroing runs deliberately outside the allocator lock. `alloc` has already taken this frame
    // off the free lists and marked it allocated with a zero refcount, so nothing else can reach
    // it, and zeroing is by far the longest thing that used to happen under that lock: a 2 MiB
    // frame measured at ~1.3 ms, during which every other cpu's frame allocation spun.
    finish_raw_alloc(frame, flags);
    Some(frame)
}

pub(super) fn raw_free_frame(frame: FrameRef) {
    if !frame.get_flags().contains(PhysicalFrameFlags::ADMITTED) {
        // TODO: this happens when a sub-frame of a larger frame is freed, even though
        // the larger frame was allocated. It'd be nice to make this not happen. But
        // if that's impossible, we can track these freed frames in a list and periodically
        // try to recover the large page if all associated pages are freed, and then free that.
        log::warn!("tried to free non-admitted frame {:?}", frame);
        return;
    }
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ADMITTED));
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
    assert!(!frame.get_flags().contains(PhysicalFrameFlags::IS_WIRED));
    check_overlap(frame, "free");
    poison_on_free(frame);
    frame.set_pt(false);
    frame.set_cow(false);
    assert_eq!(frame.refcount(), 0);
    PFA.wait().lock().free(frame);
}

/// Get a FrameRef from a physical address.
pub fn get_frame(pa: PhysAddr) -> Option<FrameRef> {
    let fi = FI.wait();
    for fi in fi {
        let f = fi.get_frame(pa);
        if f.is_some() {
            return f;
        }
    }
    None
}

pub fn split_frame(frame: FrameRef) -> (FrameRef, usize) {
    PFA.wait().lock().split_frame(frame)
}

pub fn merge_frame(frame: FrameRef) -> FrameRef {
    PFA.wait().lock().merge_frame(frame)
}

pub fn fill_stats(stats: &mut MemoryStats) {
    let pfa = PFA.wait().lock();
    for reg in &pfa.regions {
        stats.total_pages += reg.nr_pages;
        for (i, level) in reg.levels.iter().enumerate() {
            stats.levels[i].free_pages += level.free;
            stats.levels[i].page_size = PHYS_LEVEL_LAYOUTS[i].size();
            stats.levels[i].reserved_pages = 0;
            stats.levels[i].lent_pages = 0;
        }
    }
    stats.nr_levels = NR_LEVELS;
}

static BACKGROUND_ZERO_INDEX: AtomicUsize = AtomicUsize::new(0);
const MAX_BACKGROUND_ZERO_ITER: usize = 4;

pub fn background_zero_iter() -> bool {
    if needs_reschedule(false) || is_low_mem() {
        return true;
    }
    let status = BACKGROUND_ZERO_INDEX.fetch_or(1, Ordering::Acquire);
    if status & 1 == 1 {
        // another thread is already doing this
        return false;
    }
    const MAX_FRAMES_PER_REG: usize = 16;
    let pfa = PFA.wait().lock();
    let region_count = pfa.regions.len();
    drop(pfa);

    let mut frames_per_region = heapless::Vec::<
        (usize, heapless::Vec<FrameRef, MAX_FRAMES_PER_REG>),
        MAX_BACKGROUND_ZERO_ITER,
    >::new();

    let idx = status >> 1;
    for i in 0..region_count {
        if frames_per_region.is_full() || needs_reschedule(false) {
            break;
        }
        let mut pfa = PFA.wait().lock();
        let reg_idx = (idx + i) % pfa.regions.len();
        let reg = &mut pfa.regions[reg_idx];
        let mut frames_this_reg = heapless::Vec::new();
        reg.collect_zeroable(&mut frames_this_reg);
        BACKGROUND_ZERO_INDEX.fetch_add(2, Ordering::Relaxed);
        if !frames_this_reg.is_empty() {
            frames_per_region.push((reg_idx, frames_this_reg)).unwrap();
        }
    }
    BACKGROUND_ZERO_INDEX.fetch_and(!1, Ordering::Release);
    let mut total_bytes = 0;
    assert!(crate::interrupt::get());
    for (_r, frames) in &frames_per_region {
        if frames.is_empty() {
            continue;
        }
        for frame in frames {
            frame.zero();
            total_bytes += frame.size();
        }
    }

    for (reg_idx, frames) in frames_per_region {
        let mut pfa = PFA.wait().lock();
        let reg = &mut pfa.regions[reg_idx];
        for frame in frames {
            reg.free(frame);
        }
    }
    log::debug!("background zeroed {} bytes of physical memory", total_bytes);

    if total_bytes > 0 {
        crate::memory::tracker::signal_waiters();
    }

    total_bytes > 0
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use twizzler_kernel_macros::kernel_test;

    use super::{
        FrameRef, PHYS_LEVEL_LAYOUTS, PhysicalFrameFlags, get_frame, raw_alloc_frame,
        raw_free_frame, split_frame,
    };
    use crate::{
        memory::tracker::{FrameAllocFlags, free_frame, try_alloc_frames},
        thread::{entry::run_closure_in_new_thread, priority::Priority},
        utils::quick_random,
    };

    #[kernel_test]
    fn test_get_frame() {
        let frame = raw_alloc_frame(PhysicalFrameFlags::empty(), PHYS_LEVEL_LAYOUTS[0]).unwrap();
        let addr = frame.start_address();
        let test_frame = get_frame(addr).unwrap();
        assert!(core::ptr::eq(frame as *const _, test_frame as *const _));
    }

    #[kernel_test]
    fn stress_test_pmm() {
        let mut stack = Vec::new();
        for _ in 0..100000 {
            let x = quick_random();
            let y = quick_random();
            let z = quick_random();
            if x % 2 == 0 && stack.len() < 1000 {
                let frame = if y % 3 == 0 {
                    raw_alloc_frame(PhysicalFrameFlags::ZEROED, PHYS_LEVEL_LAYOUTS[0])
                } else {
                    raw_alloc_frame(PhysicalFrameFlags::empty(), PHYS_LEVEL_LAYOUTS[0])
                }
                .unwrap();
                if z % 5 == 0 {
                    frame.zero();
                }
                stack.push(frame);
            } else {
                if let Some(frame) = stack.pop() {
                    raw_free_frame(frame);
                }
            }
        }
    }

    /// One worker's round of the bulk-allocation stress below. Tags every frame and reads the
    /// tags back only after the whole batch is placed: a frame handed to two owners gets retagged
    /// in between and fails the readback, which the per-hand-out `!ALLOCATED` assert cannot see.
    fn bulk_worker(tid: u64) {
        const ITERS: usize = 300;
        let mut out: Vec<FrameRef> = Vec::new();
        for it in 0..ITERS {
            out.clear();
            let want = 1 + (quick_random() as usize % 48);
            let zeroed = it % 2 == 0;
            let flags = if zeroed {
                FrameAllocFlags::ZEROED
            } else {
                FrameAllocFlags::empty()
            };
            let got = try_alloc_frames(flags, PHYS_LEVEL_LAYOUTS[0], want, &mut out);
            assert_eq!(got, out.len());
            for (i, frame) in out.iter().enumerate() {
                assert_eq!(frame.refcount(), 0);
                assert!(frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
                assert!(frame.get_flags().contains(PhysicalFrameFlags::ADMITTED));
                assert_eq!(frame.size(), PHYS_LEVEL_LAYOUTS[0].size());
                if zeroed {
                    assert!(
                        frame.as_slice::<u64>().iter().all(|x| *x == 0),
                        "ZEROED frame {:?} contains non-zero data",
                        frame
                    );
                }
                let tag = (tid << 48) | ((it as u64) << 16) | i as u64;
                unsafe { *frame.virtaddr().as_mut_ptr::<u64>() = tag };
            }
            for (i, frame) in out.iter().enumerate() {
                let tag = (tid << 48) | ((it as u64) << 16) | i as u64;
                let seen = unsafe { *frame.virtaddr().as_ptr::<u64>() };
                assert_eq!(
                    seen, tag,
                    "frame {:?} retagged while held (double hand-out)",
                    frame
                );
            }
            // Churn the split/group-tracking path: take a large frame apart and free its
            // children one at a time, which restores a fully-free group for coalescing.
            if it % 16 == 0 {
                let mut lout: Vec<FrameRef> = Vec::new();
                if try_alloc_frames(FrameAllocFlags::empty(), PHYS_LEVEL_LAYOUTS[1], 1, &mut lout)
                    == 1
                {
                    let (head, len) = split_frame(lout[0]);
                    assert_eq!(len, PHYS_LEVEL_LAYOUTS[1].size());
                    for i in 0..(len / PHYS_LEVEL_LAYOUTS[0].size()) {
                        let f = get_frame(
                            head.start_address()
                                .offset(i * PHYS_LEVEL_LAYOUTS[0].size())
                                .unwrap(),
                        )
                        .unwrap();
                        assert_eq!(f.size(), PHYS_LEVEL_LAYOUTS[0].size());
                        free_frame(f);
                    }
                }
            }
            for frame in out.drain(..) {
                free_frame(frame);
            }
        }
    }

    #[kernel_test]
    fn stress_test_bulk_alloc() {
        const WORKERS: u64 = 3;
        let handles = (1..=WORKERS)
            .map(|tid| run_closure_in_new_thread(Priority::REALTIME, move || bulk_worker(tid)))
            .collect::<Vec<_>>();
        bulk_worker(0);
        for handle in handles {
            handle.1.wait();
        }
    }
}
