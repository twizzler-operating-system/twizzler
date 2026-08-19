use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use twizzler_rt_abi::error::{ResourceError, TwzError};

use crate::{
    memory::{
        context::virtmem::region::MapRegion,
        frame::{Frame, FrameRef, PHYS_LEVEL_LAYOUTS},
        tracker::{FrameAllocFlags, alloc_frame, free_frame, try_alloc_frame},
    },
    spinlock::Spinlock,
};

const NR_LOCKS: usize = 512;
const NR_TABLE_ENTRIES: usize = PHYS_LEVEL_LAYOUTS[0].size() / size_of::<AtomicPtr<()>>();
/// Slots one [SlotMgr] can index: two levels of [NR_TABLE_ENTRIES].
pub const MAX_SLOTS: usize = NR_TABLE_ENTRIES * NR_TABLE_ENTRIES;

/// What a slot holds, and whether an insert or a remove is partway through it.
///
/// The transitional states are the whole point. The context-wide mutex this replaces was held
/// across `map_object` and across the entire unmap, not just across the table edit -- see the
/// comments in `insert_object` and `remove_object` for what that exclusion buys. It cannot be
/// reproduced by holding a shard lock: [Spinlock] disables interrupts, and the work in between
/// takes an object's page-table lock (a *sleeping* mutex, and one that can be held by a thread
/// parked on the pager), the `secctx` mutex, and TLB shootdown waits.
///
/// So the exclusion lives in the state and the lock is held only for the transition. Readers see a
/// region only in `Present`, which is exactly what the old lock produced: a fault racing an install
/// blocked and then found nothing installed yet, and a fault racing a removal blocked and then
/// found the region gone.
enum SlotState {
    Empty,
    Installing,
    Present(Arc<MapRegion>),
    Removing,
}

/// Slot -> [MapRegion] map over a contiguous range of slot numbers, sharded so that operations on
/// different slots do not serialize against each other.
///
/// Two levels of [NR_TABLE_ENTRIES] pointers, in frames, with a boxed [SlotState] per touched
/// slot. Levels are populated on demand and never torn down short of [Drop], so a leaf pointer,
/// once non-null, stays valid and stays put.
pub struct SlotMgr {
    /// Slot numbers are indexed relative to this. Kernel-object slots start around 2^34
    /// (`KOBJ_START / MAX_SIZE`), far past what two levels can address, so they get their own
    /// manager rather than a taller tree.
    base: usize,
    len: usize,
    root_table: FrameRef,
    locks: [Spinlock<()>; NR_LOCKS],
    /// Slots in `Present`. Approximate while a transition is in flight, which is all its callers
    /// need -- the alternative is a walk of up to [MAX_SLOTS] entries taking a lock each.
    count: AtomicUsize,
}

impl SlotMgr {
    pub fn new(base: usize, len: usize) -> Self {
        assert!(len <= MAX_SLOTS);
        Self {
            base,
            len,
            root_table: alloc_frame(FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED),
            locks: [const { Spinlock::new(()) }; NR_LOCKS],
            count: AtomicUsize::new(0),
        }
    }

    /// Whether this manager is the one that answers for `slot` at all. A miss is quiet: the fault
    /// path looks user slots up in the kernel context, and has to get None rather than a panic.
    fn index(&self, slot: usize) -> Option<usize> {
        slot.checked_sub(self.base).filter(|idx| *idx < self.len)
    }

    pub fn contains(&self, slot: usize) -> bool {
        self.index(slot).is_some()
    }

    fn root_table_slice(&self) -> &'static [AtomicPtr<Frame>] {
        self.root_table.as_slice()
    }

    fn table_slice(table: *mut Frame) -> &'static [AtomicPtr<SlotState>] {
        let table: FrameRef = unsafe { &*(table as *const Frame) };
        table.as_slice()
    }

    /// The state cell for `idx`, creating the levels leading to it unless `empty_ok`.
    ///
    /// Returns null when a level is absent and `empty_ok`, or when a frame could not be allocated.
    /// Callers that must distinguish the two check memory themselves; the read paths do not care.
    fn populate(&self, idx: usize, empty_ok: bool) -> *mut SlotState {
        let root_entry = &self.root_table_slice()[idx / NR_TABLE_ENTRIES];

        let mut table = root_entry.load(Ordering::Acquire);
        if table.is_null() {
            if empty_ok {
                return core::ptr::null_mut();
            }
            // ZEROED is load-bearing, not hygiene: every entry of this table is read as a
            // `*mut SlotState` and a non-null one is taken to be a live leaf. An unzeroed table
            // makes `populate` dereference garbage on the very next line. Dropping this flag
            // while converting from `alloc_frame` cost a GPF in `user_init`'s first mapping.
            //
            // Not `alloc_frame`: that is `try_alloc_frame(..).expect("cannot wait for page")`, and
            // this runs on the map syscall path, where an out-of-memory panic is not an option.
            let Some(new_table) = try_alloc_frame(
                FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
                PHYS_LEVEL_LAYOUTS[0],
            ) else {
                return core::ptr::null_mut();
            };
            table = new_table as *const Frame as *mut Frame;
            if let Err(cur) = root_entry.compare_exchange(
                core::ptr::null_mut(),
                table,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                table = cur;
                free_frame(new_table);
            }
        }

        let entry = &Self::table_slice(table)[idx % NR_TABLE_ENTRIES];
        let mut state = entry.load(Ordering::Acquire);
        if state.is_null() {
            if empty_ok {
                return core::ptr::null_mut();
            }
            let new_state = Box::into_raw(Box::new(SlotState::Empty));
            state = new_state;
            if let Err(cur) = entry.compare_exchange(
                core::ptr::null_mut(),
                state,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                drop(unsafe { Box::from_raw(new_state) });
                state = cur;
            }
        }

        state
    }

    /// Replace the state of an already-populated slot, handing the old one back to the caller so
    /// that an `Arc<MapRegion>` never drops with the shard lock (and thus interrupts) held.
    fn swap(&self, idx: usize, new: SlotState) -> SlotState {
        let state = self.populate(idx, true);
        assert!(!state.is_null(), "slot state vanished under a guard");
        let _guard = self.locks[idx % NR_LOCKS].lock();
        core::mem::replace(unsafe { &mut *state }, new)
    }

    pub fn lookup(&self, slot: usize) -> Option<Arc<MapRegion>> {
        let idx = self.index(slot)?;
        let state = self.populate(idx, true);
        if state.is_null() {
            return None;
        }
        let _guard = self.locks[idx % NR_LOCKS].lock();
        match unsafe { &*state } {
            SlotState::Present(region) => Some(region.clone()),
            _ => None,
        }
    }

    /// Claim an empty slot, to be filled in by [InsertGuard::commit] once the mapping is built.
    ///
    /// A slot partway through an install or a removal counts as occupied, and gets the same `Busy`
    /// the occupied case has always got.
    pub fn begin_insert(&self, slot: usize) -> Result<InsertGuard<'_>, TwzError> {
        let idx = self.index(slot).ok_or(ResourceError::OutOfResources)?;
        let state = self.populate(idx, false);
        if state.is_null() {
            return Err(ResourceError::OutOfMemory.into());
        }
        {
            let _guard = self.locks[idx % NR_LOCKS].lock();
            let state = unsafe { &mut *state };
            if !matches!(state, SlotState::Empty) {
                return Err(ResourceError::Busy.into());
            }
            *state = SlotState::Installing;
        }
        Ok(InsertGuard {
            mgr: self,
            idx,
            committed: false,
        })
    }

    /// Take the region out of an occupied slot, leaving it claimed until [RemoveGuard::finish].
    ///
    /// The slot stays unavailable for the whole teardown, which is what keeps another object from
    /// being mapped into it and then having its entry removed by an unmap still in progress.
    pub fn begin_remove(&self, slot: usize) -> Option<(Arc<MapRegion>, RemoveGuard<'_>)> {
        let idx = self.index(slot)?;
        let state = self.populate(idx, true);
        if state.is_null() {
            return None;
        }
        let region = {
            let _guard = self.locks[idx % NR_LOCKS].lock();
            let state = unsafe { &mut *state };
            if !matches!(state, SlotState::Present(_)) {
                return None;
            }
            match core::mem::replace(state, SlotState::Removing) {
                SlotState::Present(region) => region,
                _ => unreachable!(),
            }
        };
        self.count.fetch_sub(1, Ordering::Relaxed);
        Some((region, RemoveGuard { mgr: self, idx }))
    }

    /// Every region this manager holds, in ascending slot order.
    ///
    /// Cold path. Null levels are skipped without taking any lock, and each region is cloned out
    /// from under the shard lock before `f` runs, so a caller that allocates cannot do it with
    /// interrupts disabled.
    pub fn for_each(&self, mut f: impl FnMut(usize, Arc<MapRegion>)) {
        for (i, root_entry) in self.root_table_slice().iter().enumerate() {
            let table = root_entry.load(Ordering::Acquire);
            if table.is_null() {
                continue;
            }
            for (j, entry) in Self::table_slice(table).iter().enumerate() {
                let state = entry.load(Ordering::Acquire);
                if state.is_null() {
                    continue;
                }
                let idx = i * NR_TABLE_ENTRIES + j;
                if idx >= self.len {
                    return;
                }
                let region = {
                    let _guard = self.locks[idx % NR_LOCKS].lock();
                    match unsafe { &*state } {
                        SlotState::Present(region) => Some(region.clone()),
                        _ => None,
                    }
                };
                if let Some(region) = region {
                    f(self.base + idx, region);
                }
            }
        }
    }

    pub fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }
}

impl Drop for SlotMgr {
    fn drop(&mut self) {
        for root_entry in self.root_table_slice() {
            let table = root_entry.load(Ordering::Acquire);
            if table.is_null() {
                continue;
            }
            for entry in Self::table_slice(table) {
                let state = entry.load(Ordering::Acquire);
                if !state.is_null() {
                    drop(unsafe { Box::from_raw(state) });
                }
            }
            free_frame(unsafe { &*(table as *const Frame) });
        }
        free_frame(self.root_table);
    }
}

/// A slot claimed by [SlotMgr::begin_insert]. Dropping without [Self::commit] releases it, so an
/// error return from the mapping work cannot strand the slot as permanently occupied.
pub struct InsertGuard<'a> {
    mgr: &'a SlotMgr,
    idx: usize,
    committed: bool,
}

impl InsertGuard<'_> {
    pub fn commit(mut self, region: Arc<MapRegion>) {
        self.mgr.swap(self.idx, SlotState::Present(region));
        self.mgr.count.fetch_add(1, Ordering::Relaxed);
        self.committed = true;
    }
}

impl Drop for InsertGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.mgr.swap(self.idx, SlotState::Empty);
        }
    }
}

/// A slot claimed by [SlotMgr::begin_remove]. Dropping frees it, so [Self::finish] is only about
/// saying where the teardown ends.
pub struct RemoveGuard<'a> {
    mgr: &'a SlotMgr,
    idx: usize,
}

impl RemoveGuard<'_> {
    pub fn finish(self) {}
}

impl Drop for RemoveGuard<'_> {
    fn drop(&mut self) {
        self.mgr.swap(self.idx, SlotState::Empty);
    }
}
