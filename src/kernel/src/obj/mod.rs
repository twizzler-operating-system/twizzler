use alloc::{
    boxed::Box,
    collections::{BTreeMap, btree_set::BTreeSet},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use intrusive_collections::{
    LinkedList, LinkedListAtomicLink, RBTreeAtomicLink, intrusive_adapter,
};
use twizzler_abi::{
    device::NUM_DEVICE_INTERRUPTS,
    meta::{MetaFlags, MetaInfo},
    object::{MAX_SIZE, ObjID, Protections},
    syscall::{BackingType, LifetimeType, ObjectInfo},
};
use twizzler_rt_abi::{bindings::object_tie, error::TwzError, object::Nonce};

pub use self::thread_sync::{SleepInfo, ThreadSleepLinker};
use crate::{
    arch::memory::frame::FRAME_SIZE,
    condvar::CondVar,
    idcounter::{IdCounter, SimpleId},
    memory::{
        VirtAddr,
        context::virtmem::region::MapRegion,
        pagetables::DeferredUnmappingOps,
        tracker::{FrameAllocFlags, alloc_frame},
    },
    mutex::{LockGuard, Mutex},
    obj::{control::VNotes, ties::TIE_MGR},
    once::{Once, OnceWait},
    random::getrandom,
    syscall::object::count_handles,
};

pub mod control;
pub mod data;
pub mod id;
pub mod omap;
pub mod pagetables;
pub mod thread_sync;
pub mod ties;

#[cfg(test)]
mod tests;

const OBJ_DELETED: u32 = 1;
pub const OBJ_HAS_INTERRUPTS: u32 = 2;
/// Created with `ObjectCreateFlags::DELETE`: delete once the last mapping goes away. Distinct from
/// `OBJ_DELETED`, which means a delete has actually been requested -- an object carrying only this
/// flag is still live, and in particular is still live before it has ever been mapped.
const OBJ_DELETE_ON_LAST_UNMAP: u32 = 4;
/// A map of this object has already tried a speculative page-in. See [Object::claim_map_prefetch].
const OBJ_MAP_PREFETCHED: u32 = 8;
/// `known_len` is an *exact* logical byte length, not a page-granular "last synced page" extent.
/// True for objects created this boot (nothing on the store until the first sync, so `known_len`
/// starts at exactly 0) and for external-file-backed objects, whose length the pager reports from
/// the backing file (`ObjectInfoFlags::SYNTH_META`). It is NOT set for a native persistent object
/// opened from the store, whose reported length is the extent of its synced pages rather than a
/// logical EOF -- past-EOF zero-fill keys on this so it never serves zeros over such an object.
const OBJ_KNOWN_LEN_EXACT: u32 = 16;
/// Backed by an external (POSIX/ext4) file, which has no Twizzler metadata page or FOT -- the pager
/// synthesizes a meta page on demand. The whole address range past `known_len` (up to, but not
/// including, the synthesized meta page) is empty, so past-EOF zero-fill needs no FOT floor and can
/// answer a fault there directly. See [Object::zero_fill_floor].
const OBJ_EXTERNAL: u32 = 32;
pub struct Object {
    pub id: ObjID,
    flags: AtomicU32,
    /// See [PtHome]. `Option` only so [Object::drop] can take them: `Some` for the whole life of
    /// every reachable object. Reach it via [Object::page_tables].
    tables: Option<Box<PtHome>>,
    /// Lazily allocated: see [`sleep_info`](Object::sleep_info).
    sleep_slot: Once<Box<Mutex<SleepInfo>>>,
    /// Threads parked anywhere in `sleep_info`, readable *without* taking that mutex.
    ///
    /// Exists for [Object::wakeup_word]: a wake that finds nobody waiting is the common case --
    /// every uncontended futex release takes it -- and discovering that used to cost a full
    /// sleeping-`Mutex` acquire (an inner spinlock, an `Arc` clone, a mutex-count pair, a critical
    /// section) to walk to an empty tree and walk back.
    ///
    /// **Ordering.** The claim is `fetch_add`ed *before* the word is read under the lock, not
    /// after the insert, and that order is the whole correctness argument. Waker and sleeper form
    /// the usual Dekker pair -- waker stores the word then loads this; sleeper increments this
    /// then loads the word -- so under `SeqCst` a waker that reads zero is ordered before the
    /// sleeper's increment, hence before its word read, so the sleeper sees the new value and
    /// declines to sleep. Incrementing after the word read inverts the pair and loses wakes.
    ///
    /// **Bias.** Every discrepancy here is deliberately upward. The overflow path in
    /// `SleepInfo::insert` drains entries via `SleepEntry::drop` without decrementing, and a
    /// `fetch_sub` underflow would wrap high rather than to zero. Both leave the count nonzero for
    /// an object that has none, which costs the fast path and nothing else; the opposite error
    /// would be a lost wakeup.
    sleepers: AtomicUsize,
    /// Lazily allocated: see [`add_device_interrupt`](Object::add_device_interrupt). Every read
    /// site is already behind `OBJ_HAS_INTERRUPTS`, which only that function sets.
    device_interrupt_info: Once<Box<[(AtomicU64, AtomicU64); NUM_DEVICE_INTERRUPTS]>>,
    /// How many contexts hold a reference to this object's page tables.
    ///
    /// Lived inside `tables` until it became the thing every delete syscall took that mutex for.
    /// Mutated *only* under the page-table lock -- every call site of [Object::inc_map_count] and
    /// [Object::dec_map_count] holds it -- so a reader that holds the lock sees the same value it
    /// always did. What the atomic buys is [is_reapable]'s negative case, which needs no lock at
    /// all: an object that is still mapped is by far the common one, and answering that used to
    /// cost the 1,280-byte sleeping `Mutex` plus the one behind `pin_info`.
    ///
    /// **Ordering.** `SeqCst`, and the delete path depends on it. A delete marks `OBJ_DELETED`
    /// and then loads this; the last unmapper stores zero (under the page-table lock) and then,
    /// after releasing that lock, loads `OBJ_DELETED` to decide whether to hand the object to the
    /// reaper. If the delete's load reads nonzero it precedes the unmapper's store of zero in the
    /// single total order, so the mark precedes it too, and the unmapper's later load is
    /// guaranteed to see the mark. Every object therefore falls to exactly one of the two paths,
    /// never to neither. Weaker orderings break that argument, which is the whole reason the fast
    /// path is safe to take without the lock.
    map_count: AtomicUsize,
    pin_info: Mutex<PinInfo>,
    lifetime_type: LifetimeType,
    ties: Vec<object_tie>,
    verified_id: OnceWait<(bool, Protections)>,
    /// The backing store's data length. `u64::MAX` means "never told"; see [Object::known_len].
    known_len: AtomicU64,
    vnotes: VNotes,
    /// Link into the sharded object map ([omap::ShardedOmap]); unused unless [OMAP_SHARDED].
    omap_link: RBTreeAtomicLink,
    /// Link into the reaper's object queue ([ReapQueue::objs]), which holds a reference while
    /// linked -- so this is always unlinked by the time [Object::drop] runs.
    reap_link: LinkedListAtomicLink,
    /// The regions mapping this object, keyed by slot.
    ///
    /// `Weak`, not `Arc`: a [MapRegion] holds an [ObjectRef], so a strong reference here would be
    /// a cycle -- the object and every frame behind it would stay alive forever on any path that
    /// dropped the last outside reference without a matching [Object::remove_mapping]. The
    /// context's slot manager owns the region; this is an index into it.
    ///
    /// Lock order: a sleeping mutex, so never taken under a slot shard spinlock, and taken before
    /// `tables` where the two meet.
    mappings: Mutex<BTreeMap<usize, Weak<MapRegion>>>,
}

/// An object's page tables, in an allocation of their own.
///
/// Tearing them down unmaps the object's whole range, runs TLB consistency, and frees every frame
/// with a `WAIT_OK` allocator -- it can sleep waiting for memory. [Object::drop] runs on whoever
/// released the last reference, including the pager completion thread, which that sleep wedges on
/// a resource it is itself needed to replenish (`sysbench.md` F7). So the drop hands them over,
/// and the handover must neither block nor allocate.
///
/// Hence the separate allocation: it outlives the object, giving the reaper an address to thread
/// a list through. `grave_link` sits outside the mutex because the reaper links it under a
/// spinlock, which may not take a sleeping mutex.
struct PtHome {
    grave_link: LinkedListAtomicLink,
    tables: Mutex<pagetables::ObjectPageTable>,
}

impl PtHome {
    fn new() -> Self {
        Self {
            grave_link: LinkedListAtomicLink::new(),
            tables: Mutex::new(pagetables::ObjectPageTable::new()),
        }
    }
}

intrusive_adapter!(ReapAdapter = ObjectRef: Object { reap_link: LinkedListAtomicLink });
intrusive_adapter!(GraveAdapter = Box<PtHome>: PtHome { grave_link: LinkedListAtomicLink });

impl Drop for Object {
    fn drop(&mut self) {
        if self.use_pager() && self.is_pending_delete() {
            // Queued, never issued here: this runs on whichever thread happens to drop the last
            // reference, which includes the pager completion thread and threads holding spinlocks.
            // See `pager::Deleter`.
            crate::pager::queue_del_object(self.id);
        }
        // The reap queue holds a reference for as long as an entry is linked, so reaching this
        // drop means we are off it.
        debug_assert!(!self.reap_link.is_linked());
        // Same reason as the delete above, and the larger half of it -- see [PtHome]. Everything
        // else here is bounded frees (sleep trees, notes, the interrupt array), so it stays inline
        // rather than growing a second handover for work that cannot block.
        if let Some(home) = self.tables.take() {
            defer_teardown(home);
        }
    }
}

#[derive(Default)]
struct PinInfo {
    id_counter: IdCounter,
    pins: Vec<SimpleId>,
}

#[derive(Clone, Copy, Debug, PartialOrd, Ord, PartialEq, Eq)]
#[repr(transparent)]
pub struct PageNumber(usize);

impl core::ops::Add for PageNumber {
    type Output = usize;

    fn add(self, rhs: Self) -> Self::Output {
        self.0 + rhs.0
    }
}

impl core::ops::Sub for PageNumber {
    type Output = usize;

    fn sub(self, rhs: Self) -> Self::Output {
        self.0 - rhs.0
    }
}

impl PageNumber {
    pub fn num(&self) -> usize {
        self.0
    }

    pub const PAGE_SIZE: usize = FRAME_SIZE;

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    pub fn is_meta(&self) -> bool {
        self.as_byte_offset() == MAX_SIZE - Self::PAGE_SIZE
    }

    pub fn meta_page() -> Self {
        Self((MAX_SIZE - Self::PAGE_SIZE) / Self::PAGE_SIZE)
    }

    pub fn base_page() -> Self {
        Self(1)
    }

    pub fn as_byte_offset(&self) -> usize {
        self.0 * Self::PAGE_SIZE
    }

    pub fn from_address(addr: VirtAddr) -> Self {
        PageNumber((addr.raw() as usize % MAX_SIZE) / Self::PAGE_SIZE)
    }

    pub fn from_offset(off: usize) -> Self {
        PageNumber(off / Self::PAGE_SIZE)
    }

    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }

    pub fn prev(&self) -> Option<Self> {
        if self.0 == 0 {
            None
        } else {
            Some(Self(self.0 - 1))
        }
    }

    pub fn offset(&self, off: usize) -> Self {
        Self(self.0 + off)
    }

    pub fn byte_offset(&self, off: usize) -> Self {
        Self(self.0 + off / Self::PAGE_SIZE)
    }

    pub fn align_down(&self, align: usize) -> Self {
        Self(self.0 & !(align - 1))
    }
}

impl From<usize> for PageNumber {
    fn from(x: usize) -> Self {
        Self(x)
    }
}

impl Object {
    pub fn is_pending_delete(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & OBJ_DELETED != 0
    }

    pub fn is_mapped(&self) -> bool {
        self.map_count() > 0
    }

    /// How many contexts map this object. See the field for why this needs no lock.
    pub fn map_count(&self) -> usize {
        self.map_count.load(Ordering::SeqCst)
    }

    /// Current sleeper claim count, for the hang report. `wakeup_word`'s fast path returns without
    /// waking when this reads zero, so a parked thread in this object's sleep tree alongside a zero
    /// here is that skip's smoking gun; the count is biased upward (see the field), so zero is the
    /// one value it must never show while anyone is parked.
    pub fn sleeper_count(&self) -> usize {
        self.sleepers.load(Ordering::SeqCst)
    }

    /// Caller must hold this object's page-table lock; see the [`map_count`](Object::map_count)
    /// field. Paired one-for-one with [Object::dec_map_count] by the arch mapper's `took_ref`.
    pub fn inc_map_count(&self) {
        self.map_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Returns the new count, so the "last mapping just went away" test costs no second load.
    /// Caller must hold this object's page-table lock.
    pub fn dec_map_count(&self) -> usize {
        let prev = self.map_count.fetch_sub(1, Ordering::SeqCst);
        assert!(prev > 0, "map count cannot be negative");
        prev - 1
    }

    pub fn get_notes(&self) -> &VNotes {
        &self.vnotes
    }

    pub fn use_pager(&self) -> bool {
        self.lifetime_type == LifetimeType::Persistent
    }

    /// How far the backing store's data extends, in bytes, or `None` if the kernel has never been
    /// told.
    ///
    /// Authoritative rather than advisory: the store's length changes only when the kernel syncs
    /// pages to the pager, so the kernel is the only writer and can keep its own copy exact. That
    /// is what makes it safe to answer a fault past this point *without asking the pager* -- there
    /// is provably nothing out there to read.
    ///
    /// The error direction is what matters. Overstating the length costs a pager round trip for a
    /// page the kernel could have zero-filled; understating it would serve zeros over real data. So
    /// every update moves it forward only ([Object::extend_known_len]), and it is set before a sync
    /// is confirmed rather than after.
    pub fn known_len(&self) -> Option<u64> {
        match self.known_len.load(Ordering::Acquire) {
            u64::MAX => None,
            len => Some(len),
        }
    }

    /// Record the store's length, as reported by the pager for an object it already had, or zero
    /// for one the kernel just created (nothing is on disk until the first sync).
    pub fn set_known_len(&self, len: u64) {
        self.known_len.store(len, Ordering::Release);
    }

    /// Mark this object's `known_len` as an exact logical byte length (see [OBJ_KNOWN_LEN_EXACT]).
    pub fn mark_known_len_exact(&self) {
        self.flags.fetch_or(OBJ_KNOWN_LEN_EXACT, Ordering::SeqCst);
    }

    /// Whether `known_len` is an exact logical length rather than a synced-page extent.
    pub fn known_len_is_exact(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & OBJ_KNOWN_LEN_EXACT != 0
    }

    /// Mark this object as backed by an external file (no metadata page / FOT).
    pub fn mark_external(&self) {
        self.flags.fetch_or(OBJ_EXTERNAL, Ordering::SeqCst);
    }

    /// Whether this object is backed by an external file (see [OBJ_EXTERNAL]).
    pub fn is_external(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & OBJ_EXTERNAL != 0
    }

    /// Grow the recorded length to cover `len`, leaving it alone if it already does.
    pub fn extend_known_len(&self, len: u64) {
        let _ = self
            .known_len
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                (cur != u64::MAX && cur < len).then_some(len)
            });
    }

    pub fn is_kernel_id(&self) -> bool {
        self.id.parts()[0] == 1
    }

    /// True for exactly one caller, ever: the first map of this object to reach the speculative
    /// page-in. Concurrent maps of one object are ordinary (four threads opening the same library),
    /// so this has to be the atomic and not a read followed by a set.
    pub fn claim_map_prefetch(&self) -> bool {
        self.flags.fetch_or(OBJ_MAP_PREFETCHED, Ordering::SeqCst) & OBJ_MAP_PREFETCHED == 0
    }

    /// Record that this object should be deleted once its last mapping goes away.
    pub fn set_delete_on_last_unmap(&self) {
        self.flags
            .fetch_or(OBJ_DELETE_ON_LAST_UNMAP, Ordering::SeqCst);
    }

    /// Called after a mapping is torn down and the map count has reached zero. Objects created
    /// with `ObjectCreateFlags::DELETE` become deletable exactly here -- not at creation, where
    /// they have no mappings only because the creator has not mapped them yet.
    pub fn note_last_unmap(&self) {
        if self.flags.load(Ordering::SeqCst) & OBJ_DELETE_ON_LAST_UNMAP != 0 {
            self.mark_for_delete();
        }
    }

    #[track_caller]
    pub fn mark_for_delete(&self) {
        record_delete(self.id, core::panic::Location::caller());
        self.flags.fetch_or(OBJ_DELETED, Ordering::SeqCst);
    }

    #[track_caller]
    pub fn lock_page_tables(&self) -> PtGuard<'_> {
        PtGuard::new(self.page_tables())
    }

    pub fn add_mapping(&self, slot: usize, region: &Arc<MapRegion>) {
        self.mappings.lock().insert(slot, Arc::downgrade(region));
    }

    pub fn remove_mapping(&self, slot: usize) {
        self.mappings.lock().remove(&slot);
    }

    /// The live regions mapping this object, pruning any whose region has gone away without a
    /// matching [Self::remove_mapping].
    pub fn mappings(&self) -> Vec<Arc<MapRegion>> {
        let mut mappings = self.mappings.lock();
        let mut out = Vec::with_capacity(mappings.len());
        mappings.retain(|_, region| match region.upgrade() {
            Some(region) => {
                out.push(region);
                true
            }
            None => false,
        });
        out
    }

    /// This object's page tables. See the field for why it is a boxed `Option`.
    fn page_tables(&self) -> &Mutex<pagetables::ObjectPageTable> {
        &self
            .tables
            .as_ref()
            .expect("page tables taken from an object that is still reachable")
            .tables
    }

    pub fn id(&self) -> ObjID {
        self.id
    }

    pub fn release_pin(&self, _pin: u32) {
        // TODO: Currently we don't track pins. This will be changed in-future when we fully
        // implement eviction.
    }

    #[track_caller]
    pub fn new(id: ObjID, lifetime_type: LifetimeType, ties: &[object_tie]) -> Self {
        log::trace!(
            "creating new object {} with lifetime {:?} and {} ties from {}",
            id,
            lifetime_type,
            ties.len(),
            core::panic::Location::caller()
        );
        use crate::syscall::object::createprofile as cp;
        let t = cp::start();
        let device_interrupt_info = Once::new();
        let sleep_slot = Once::new();
        if OBJ_EAGER_COLD_FIELDS {
            device_interrupt_info.call_once(|| {
                Box::new([const { (AtomicU64::new(0), AtomicU64::new(0)) }; NUM_DEVICE_INTERRUPTS])
            });
            sleep_slot.call_once(|| Box::new(Mutex::new(SleepInfo::new(id))));
        }
        cp::record(cp::Stage::NewDevBox, t);
        let t = cp::start();
        let this = Self {
            id,
            flags: AtomicU32::new(0),
            tables: Some(Box::new(PtHome::new())),
            sleep_slot,
            sleepers: AtomicUsize::new(0),
            map_count: AtomicUsize::new(0),
            pin_info: Mutex::new(PinInfo::default()),
            ties: ties.to_vec(),
            verified_id: OnceWait::new(),
            known_len: AtomicU64::new(u64::MAX),
            lifetime_type,
            device_interrupt_info,
            vnotes: VNotes::new(),
            omap_link: RBTreeAtomicLink::new(),
            reap_link: LinkedListAtomicLink::new(),
            mappings: Mutex::new(BTreeMap::new()),
        };
        cp::record(cp::Stage::NewStruct, t);
        this
    }

    /// This object's sleep-word table, built on first use.
    ///
    /// Every object used to carry one inline, and it is 1,920 bytes -- 45% of the 4,288-byte
    /// `Object` -- almost all of it a 32-slot `FnvIndexMap` that stays empty for every object that
    /// nobody ever sleeps on. `Arc::new(Object)` measured 1,017 ns of the 6.1 us
    /// `sys_object_create` costs, and it is a heap allocation plus a memcpy of exactly this
    /// struct.
    ///
    /// The allocation lands on the first *sleep*, which is a path that is about to block anyway.
    /// The wake path never reaches here on an object with no sleepers: `wakeup_word` returns at its
    /// `sleepers == 0` check, which is the same guard that already existed to keep an uncontended
    /// futex release out of this mutex.
    pub(crate) fn sleep_info(&self) -> &Mutex<SleepInfo> {
        self.sleep_slot.call_once(|| {
            coldfieldstats::SLEEP_INITS.fetch_add(1, Ordering::Relaxed);
            Box::new(Mutex::new(SleepInfo::new(self.id)))
        })
    }

    /// This object's device-interrupt table, built on first use.
    ///
    /// Only device KSOs ever have one; it is 512 bytes and a separate allocation, measured at
    /// 358 ns of every `sys_object_create`. Both read sites are gated on `OBJ_HAS_INTERRUPTS`,
    /// which nothing but `add_device_interrupt` sets, and it sets it after building this -- so a
    /// reader that passes the gate finds the table already there and never allocates.
    pub(crate) fn device_interrupt_table(
        &self,
    ) -> &[(AtomicU64, AtomicU64); NUM_DEVICE_INTERRUPTS] {
        self.device_interrupt_info.call_once(|| {
            coldfieldstats::DEV_INITS.fetch_add(1, Ordering::Relaxed);
            Box::new([const { (AtomicU64::new(0), AtomicU64::new(0)) }; NUM_DEVICE_INTERRUPTS])
        })
    }

    /// The sleep-word table if it exists, without building one.
    ///
    /// For paths that only remove or wake: an object that has never been slept on has nothing for
    /// them to find, and allocating to discover that would be backwards.
    pub(crate) fn sleep_info_if_present(&self) -> Option<&Mutex<SleepInfo>> {
        self.sleep_slot.poll().map(|b| &**b)
    }

    pub fn new_kernel_with_id(id: ObjID) -> Arc<Self> {
        let obj = Self::new(id, LifetimeType::Volatile, &[]);
        let meta = MetaInfo {
            nonce: Nonce(0),
            kuid: 0.into(),
            default_prot: Protections::all(),
            flags: MetaFlags::empty(),
            fotcount: 0,
            extcount: 0,
        };
        let obj = Arc::new(obj);
        obj.init_meta(meta);
        obj
    }

    /// Install the metadata page of an object that has just been constructed.
    ///
    /// [`Object::write_meta`] goes through the generic fill path -- `lock_page_tables` ->
    /// `ensure_in_core` -> `with_frame` -- which exists to handle pages that may already be
    /// present, may be COW, may belong to a pager-backed object, and may be raced for by a fault
    /// on another cpu. None of that can apply here: the object was built moments ago by this
    /// thread, is not registered, is not mapped, has no pager, and has no frames at all. Measured
    /// at ~8.8 us against ~1.9 us for allocating the frame and installing it directly, which is
    /// what [`super::control::ControlObjectCacher`] already does for the base page one call later,
    /// and it is paid once per kernel object -- so once per thread spawned.
    ///
    /// Frame flags deliberately match what `ensure_in_core` would have used, so the frame is
    /// accounted and reclaimed identically. It is not wired: nothing keeps a pointer to it.
    ///
    /// `sys_object_create` qualifies on the same terms and takes it too, which is where the cost
    /// actually lands: 8.0 us of a 14.8 us `ObjectCreate` (`sysbench.md`).
    pub(crate) fn init_meta(self: &Arc<Self>, meta: MetaInfo) {
        use crate::syscall::object::createprofile as cp;
        let t = cp::start();
        let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK);
        cp::record(cp::Stage::MetaFrame, t);
        // Safety: a freshly allocated frame, named by nothing else, and `MetaInfo` sits at offset
        // zero of the meta page -- the offset `write_meta` writes it to.
        unsafe { frame.virtaddr().as_mut_ptr::<MetaInfo>().write(meta) };
        let t = cp::start();
        self.add_frame(PageNumber::meta_page(), frame);
        cp::record(cp::Stage::MetaAdd, t);
        let t = cp::start();
        self.note_written_meta(&meta);
        cp::record(cp::Stage::MetaNote, t);
    }

    pub fn new_kernel() -> Arc<Self> {
        let mut bytes = [0; 16];
        if !getrandom(&mut bytes, true) {
            let meta = MetaInfo {
                nonce: Nonce(0),
                kuid: 0.into(),
                default_prot: Protections::all(),
                flags: MetaFlags::empty(),
                fotcount: 0,
                extcount: 0,
            };
            let obj = Arc::new(Self::new(id::backup_id_gen(), LifetimeType::Volatile, &[]));
            obj.init_meta(meta);
            return obj;
        }
        let nonce = u128::from_ne_bytes(bytes);
        let obj = Self::new(
            id::calculate_new_id(0.into(), MetaFlags::default(), nonce, Protections::all()),
            LifetimeType::Volatile,
            &[],
        );
        let meta = MetaInfo {
            nonce: Nonce(nonce),
            kuid: 0.into(),
            default_prot: Protections::all(),
            flags: MetaFlags::empty(),
            fotcount: 0,
            extcount: 0,
        };
        let obj = Arc::new(obj);
        obj.init_meta(meta);
        obj
    }

    pub fn print_page_tree(&self) {
        logln!("=== PAGE TREE OBJECT {} ===", self.id());
        self.lock_page_tables().print_tree();
    }

    pub fn info(&self) -> ObjectInfo {
        let (num_pages, maps) = {
            let page_tree = self.lock_page_tables();
            (page_tree.count_pages(), self.map_count())
        };
        ObjectInfo {
            id: self.id,
            maps,
            // TODO: see TIE_MGR
            ties_to: 0,
            ties_from: 0,
            life: self.lifetime_type,
            backing: BackingType::default(),
            pages: num_pages,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum InvalidateMode {
    Full,
    WriteProtect,
}

impl Display for PageNumber {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Object {}

impl PartialOrd for Object {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for Object {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl core::fmt::Debug for Object {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Object({})", self.id())
    }
}

/// A held object page-table lock, which on release discharges whatever the operations under it
/// parked -- waiting for their TLB shootdowns and then freeing the frames those shootdowns protect.
///
/// The order in `drop` is the whole point and is not interchangeable: the mutex is released
/// *before* the wait. That is what takes a median 90 ms per boot of shootdown spinning out of this
/// lock's hold (TLB.md).
///
/// What makes releasing first *safe* is not that `run_all` waits before it frees -- that only
/// covers the path where `run_all` is called. It is that [DeferredUnmappingOps] has a backstop for
/// each of its fields on the path where it isn't: a non-empty `pages` trips its `Drop` assert, and
/// `pending` discharges through `PendingShootdown`'s own `Drop`. So a parked value that is dropped
/// rather than run can neither free early nor skip the wait -- it panics or it waits. A field added
/// to that struct without a comparable backstop would silently break this.
///
/// Every path that locks an [ObjectPageTable] must produce one of these rather than a bare
/// `LockGuard`, or parked work sits until some later guard happens to pick it up.
pub struct PtGuard<'a> {
    /// `Option` only so that `drop` can release the mutex before running the parked work.
    inner: Option<LockGuard<'a, pagetables::ObjectPageTable>>,
}

impl<'a> PtGuard<'a> {
    #[track_caller]
    pub fn new(m: &'a Mutex<pagetables::ObjectPageTable>) -> Self {
        Self {
            inner: Some(m.lock()),
        }
    }

    /// Two objects' page tables at once, in the address order [crate::utils::lock_two] uses to keep
    /// deadlock cycles from forming.
    ///
    /// Exists so the two-object paths cannot quietly take bare `LockGuard`s: work parked under one
    /// of those would sit undischarged until some later guard on the same object picked it up,
    /// which is a delay with no bound rather than a compile error.
    pub fn new_two<'b>(
        a: &'a Mutex<pagetables::ObjectPageTable>,
        b: &'b Mutex<pagetables::ObjectPageTable>,
    ) -> (Self, PtGuard<'b>) {
        let (ga, gb) = crate::utils::lock_two(a, b);
        (Self { inner: Some(ga) }, PtGuard { inner: Some(gb) })
    }

    /// Release two page-table locks, and only then discharge what either of them parked.
    ///
    /// Required wherever two are held at once, and not merely tidier. Rust's drop order releases
    /// the inner guard first, so its parked work -- a shootdown wait, now preemptible, plus frame
    /// frees -- would run with the outer lock still held. That is exactly the nested-hold shape
    /// this change exists to remove, arriving by implicit drop order rather than by any decision in
    /// the code. There is no ordering of the two drops that avoids it: releasing the outer first
    /// runs *its* parked work under the inner. The release has to be explicit.
    ///
    /// An early `?` between locking and this call falls back to drop order, which is correct but
    /// nested; that is the error path and is left alone deliberately.
    pub fn release_two(mut a: Self, mut b: PtGuard<'_>) {
        let (ops_a, ops_b) = (a.drain(), b.drain());
        drop(a);
        drop(b);
        if let Some(ops) = ops_a {
            ops.run_all();
        }
        if let Some(ops) = ops_b {
            ops.run_all();
        }
    }

    fn drain(&mut self) -> Option<DeferredUnmappingOps> {
        self.inner.as_mut().unwrap().take_deferred()
    }
}

impl core::ops::Deref for PtGuard<'_> {
    type Target = pagetables::ObjectPageTable;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl core::ops::DerefMut for PtGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl Drop for PtGuard<'_> {
    fn drop(&mut self) {
        let mut guard = self.inner.take().unwrap();
        let ops = guard.take_deferred();
        drop(guard);
        if let Some(ops) = ops {
            ops.run_all();
        }
    }
}

pub type ObjectRef = Arc<Object>;

/// Selects the sharded object map ([omap::ShardedOmap]) over the single global
/// `Mutex<BTreeMap>`. Both are compiled; one tree state builds both A/B arms.
pub const OMAP_SHARDED: bool = true;

struct ObjectManager {
    map: Mutex<BTreeMap<ObjID, ObjectRef>>,
    sharded: omap::ShardedOmap,
    no_exist: Mutex<BTreeSet<ObjID>>,
}

bitflags::bitflags! {
    #[derive(Debug)]
    pub struct LookupFlags: u32 {
        const ALLOW_DELETED = 1;
    }
}

#[derive(Debug, Clone)]
pub enum LookupResult {
    NotFound,
    WasDeleted,
    Pending,
    Found(ObjectRef),
}

impl LookupResult {
    pub fn unwrap(self) -> ObjectRef {
        if let Self::Found(o) = self {
            o
        } else {
            panic!("unwrap LookupResult failed: {:?}", self)
        }
    }

    pub fn ok_or<E>(self, e: E) -> Result<ObjectRef, E> {
        if let Self::Found(o) = self {
            Ok(o)
        } else {
            Err(e)
        }
    }
}

impl ObjectManager {
    fn new() -> Self {
        Self {
            map: Mutex::new(BTreeMap::new()),
            sharded: omap::ShardedOmap::new(),
            no_exist: Mutex::new(BTreeSet::new()),
        }
    }

    fn lookup_object(&self, id: ObjID, _flags: LookupFlags) -> LookupResult {
        if OMAP_SHARDED {
            // The positive map answers first. `no_exist` is a negative cache, and nothing ever
            // removes an entry from it -- so consulting it first both charged every successful
            // lookup a second global lock and permanently shadowed any object whose id was
            // marked nonexistent before it came to exist.
            if let Some(obj) = self.sharded.lookup(id) {
                return LookupResult::Found(obj);
            }
            if self.no_exist.lock().contains(&id) {
                return LookupResult::NotFound;
            }
        } else {
            if self.no_exist.lock().contains(&id) {
                return LookupResult::NotFound;
            }
            if let Some(res) = self
                .map
                .lock()
                .get(&id)
                .map(|obj| LookupResult::Found(obj.clone()))
            {
                return res;
            }
        }
        ties::TIE_MGR
            .lookup_object(id)
            .map_or(LookupResult::NotFound, |obj| LookupResult::Found(obj))
    }

    fn register_object(&self, obj: Arc<Object>) {
        if OMAP_SHARDED {
            // An evicted duplicate (same-id replacement) drops here, outside the shard lock.
            drop(self.sharded.insert(obj));
        } else {
            // TODO: what if it returns an obj
            self.map.lock().insert(obj.id(), obj);
        }
    }
}

pub fn print_all_objects() {
    let mgr = obj_manager();
    // Both arms print from a collected snapshot: the sharded map's locks are spinlocks, which
    // must not be held across console output.
    let objs: Vec<ObjectRef> = if OMAP_SHARDED {
        let mut v = Vec::new();
        mgr.sharded.collect_all(&mut v);
        v
    } else {
        mgr.map.lock().values().cloned().collect()
    };
    logln!("=== OBJECTS === ({})", objs.len());
    let mut nn = 0;
    for obj in objs.iter() {
        logln!("{}: {:?}", obj.id, obj.info());
        obj.print_notes();
        if obj.enumerate_notes(0, 1).is_empty() {
            nn += 1;
        }
    }
    logln!("\n=== OBJECTS WITH NO NOTES === ({})\n\n", nn);

    nn = 0;
    TIE_MGR.with_deleted_map(|deleted| {
        logln!("=== DELETED OBJECTS === ({})", deleted.len());
        for (id, obj) in deleted.iter() {
            logln!("{}: {:?}", id, obj.info());
            obj.print_notes();
            if obj.enumerate_notes(0, 1).is_empty() {
                nn += 1;
            }
        }
    });
    logln!("\n=== DELETED OBJECTS WITH NO NOTES === ({})\n\n", nn);
}

/// A/B knob for the lock-free negative in [`is_reapable`].
///
/// `false` restores taking the page-table lock to read the map count, which is what every
/// measurement before this change was taken against. The count itself is atomic either way, so
/// this isolates *skipping the lock* rather than restoring the old field.
pub const OBJ_REAP_MAP_COUNT_FAST: bool = false;

/// Whether `obj` can be reaped now: nothing maps it and nothing has it pinned.
///
/// Takes the object's pin lock, and the page-table lock too unless
/// [`OBJ_REAP_MAP_COUNT_FAST`] elides it, so it must be called with the global map lock
/// *released* -- see [`scan_deleted`] for what deadlocks otherwise.
fn is_reapable(obj: &ObjectRef) -> bool {
    // A mapped object is the common case on this path -- `ObjectControlCmd::Delete` runs it on
    // every delete, and the bench pattern deletes while still mapped -- and it is answerable
    // without either lock.
    if OBJ_REAP_MAP_COUNT_FAST {
        if obj.map_count() != 0 && !stale_map_count(obj) {
            return false;
        }
        return obj.pin_info.lock().pins.len() == 0;
    }
    let _tables = obj.lock_page_tables();
    (obj.map_count() == 0 || stale_map_count(obj)) && obj.pin_info.lock().pins.len() == 0
}

/// A positive map count with no live mapping is stale accounting, not reachability: installs
/// only happen through a registered `MapRegion` (registration now precedes the install), so an
/// object nothing maps cannot be faulted back in, whatever its count says. Bulk arch teardown
/// paths can swallow decs -- measured as ~7.3k pending-delete objects (one ~4MB heap span per
/// dead compartment, 8.4GB) stranded at count 1 in many-reclaim12..18 -- and this predicate is
/// what keeps any such miss from becoming a permanent leak. Counted so a healthy boot can prove
/// how often the escape hatch fires.
fn stale_map_count(obj: &ObjectRef) -> bool {
    if !obj.mappings().is_empty() {
        return false;
    }
    reapstats::STALE_COUNT_REAPS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Remove `obj` from the map and hand it to the tie manager.
///
/// Re-checks under the map lock: the predicate above was evaluated unlocked, so the entry may have
/// been replaced or resurrected since. The removed reference is dropped by the tie manager rather
/// than under the map lock.
fn reap_one(obj: &ObjectRef) {
    let dobj = if OMAP_SHARDED {
        obj_manager().sharded.remove_if_pending(obj.id, obj)
    } else {
        let mut om = obj_manager().map.lock();
        let unchanged = om
            .get(&obj.id)
            .is_some_and(|cur| Arc::ptr_eq(cur, obj) && cur.is_pending_delete());
        if unchanged { om.remove(&obj.id) } else { None }
    };

    if let Some(dobj) = dobj {
        ties::TIE_MGR.delete_object(dobj);
    }
}

/// The reap rework (`sysbench.md` F6). On: `ObjectControlCmd::Delete` reaps only the object it just
/// marked, and the unmap paths hand the reaper thread the object whose last mapping went away. Off,
/// delete runs a full [`scan_deleted`] inline -- a walk of every object in the system, taking each
/// one's page-table lock -- and the unmap paths say nothing.
///
/// Worth 16.1 us of the 16.6 us a delete used to cost, and ~7 us of `object_create_delete`.
///
/// This wedged the machine for as long as it was on, deterministically, and the cause was not
/// here: prompt reaping merely made it far more likely that an object's last `ObjectRef` died on
/// the pager completion thread, where `Object::drop`'s blocking delete parked the one thread that
/// drains completions (see `pager::Deleter`). Bisecting to this switch found the trigger, not the
/// defect -- which is why three fixes aimed here in turn each still wedged. With the drop deferred,
/// the sysbench suite passes 5/5 at smp1 and 5/5 at smp4, where it used to stop dead 5 out of 5.
pub const TARGETED_REAP: bool = true;

/// Background reaper: tears down what the unmap paths and dying objects hand it.
///
/// Off those paths because both halves of its work can block. Reaping walks candidates taking each
/// one's page-table and pin locks, against the very paths doing the unmapping, and issues deletes
/// to the userspace pager; tearing down page tables frees frames with a waiting allocator. It also
/// cannot be left to the idle loop's scan, which never runs while a cpu stays busy: without any
/// reaper, a create/map/delete/unmap loop retained every object it made and exhausted memory
/// partway through the suite.
struct Reaper {
    work: CondVar,
    queue: crate::spinlock::Spinlock<ReapQueue>,
}

/// The reaper's two queues, and the state its wake decision reads.
///
/// Intrusive because a push happens under this spinlock, from [Object::drop] on whatever thread
/// released the last reference. The `Vec` pair this replaced allocated there -- reaching the heap
/// allocator, `GLOBAL_PAGE_ALLOC` and a shootdown-waiting `arch.map` with interrupts off, and able
/// to fail for want of memory on the path whose job is to hand memory back. Links live in memory
/// the queued thing already owns, so these grow unbounded without asking for a byte, and the cap
/// the old queue needed goes away with the allocation.
///
/// Behind the lock the thread waits on, not in atomics beside it: `CondVar::wait` registers the
/// waiter before releasing the guard, so a producer either enqueues before the thread tests the
/// queues or signals after it is queued. Tested-then-signalled without the lock loses wakeups.
struct ReapQueue {
    /// Objects an unmap may have made reapable, one reference held per entry.
    ///
    /// A queue, not a "something changed" flag: a flag makes the thread run [`scan_deleted`], and
    /// under a workload that unmaps constantly that is a walk of every object in the system --
    /// taking each one's page-table lock -- on a loop, against the very paths doing the
    /// unmapping. The contended-sync bench wedged outright that way. What the unmap paths know is
    /// *which* object became reapable, so they say so and the thread checks only those.
    ///
    /// At most one entry per object, enforced by [Object::reap_link] -- mandatory, since
    /// inserting a linked node panics. Skipping a duplicate is sound because the queued entry is
    /// examined strictly after the skipped push, so it sees whatever prompted it.
    ///
    /// It is *not* why the old `MAX_REAP_QUEUE` cap could go: measured, duplicates are ~7 pushes
    /// per boot against ~495,000 (`deduped=` on the reaper stats line), so the dedupe bound is
    /// real but almost never exercised. The cap existed to bound an allocating `Vec`; the list
    /// allocates nothing, so there is nothing left to cap. A burst of *distinct* objects -- the
    /// case an overflow path usually exists for -- is bounded only by the pending-delete set,
    /// which is exactly what [`scan_deleted`] would have walked anyway.
    objs: LinkedList<ReapAdapter>,
    /// Page tables handed over by [Object::drop]. See [PtHome].
    graves: LinkedList<GraveAdapter>,
    /// Entries across both queues. Maintained rather than counted, because `LinkedList::len` is a
    /// walk; read only by [`should_signal_reaper`].
    depth: usize,
    /// Whether the reaper thread is blocked in [`Reaper::work`] rather than working.
    parked: bool,
}

impl ReapQueue {
    const fn new() -> Self {
        Self {
            objs: LinkedList::new(ReapAdapter::NEW),
            graves: LinkedList::new(GraveAdapter::NEW),
            depth: 0,
            parked: false,
        }
    }
}

static REAPER: Once<Reaper> = Once::new();

pub fn start_reaper_thread() {
    extern "C" fn reaper_entry() {
        let r = REAPER.wait();
        let mut q = r.queue.lock();
        loop {
            // Popped, not `take`n: `take` moves head and tail and touches no node, leaving
            // drained entries still `is_linked()`, which producers read as "already queued".
            //
            // Graves lead, because they hold frames -- but only one per object, never drained to
            // empty. `Object::drop` outruns `request_reap` ~2.8:1, so absolute grave priority
            // starves the object queue. The batch this replaced could not starve: it worked from
            // a snapshot of both.
            let grave = q.graves.pop_front();
            let obj = q.objs.pop_front();
            if grave.is_some() || obj.is_some() {
                q.depth -= grave.is_some() as usize + obj.is_some() as usize;
                // Both halves block -- a grave frees frames with a waiting allocator, a reap
                // waits on the pager -- and dropping the last `ObjectRef` re-enters
                // `defer_teardown`, which takes this lock.
                drop(q);
                if grave.is_some() {
                    drop(grave);
                    reapstats::DRAINED_GRAVES.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(obj) = obj {
                    scan_deleted_one(&obj);
                    drop(obj);
                    reapstats::DRAINED_OBJS.fetch_add(1, Ordering::Relaxed);
                }
                q = r.queue.lock();
                continue;
            }
            // Both queues are empty here, so `depth` must be exactly zero. Checked rather than
            // assumed because `OBJ_REAP_BATCH_WAKE` makes `depth` gate signal suppression: a
            // drift would silently defeat batching (always signal) or stall it (never signal),
            // with no other symptom and no test between here and there. Counted and corrected
            // rather than asserted, so it reports from a release boot -- `debug_assert` is
            // compiled out of this repo's release profile.
            if q.depth != 0 {
                reapstats::DEPTH_DRIFT.fetch_add(1, Ordering::Relaxed);
                q.depth = 0;
            }
            q.parked = true;
            q = r.work.wait(q);
            q.parked = false;
            reapstats::BATCHES.fetch_add(1, Ordering::Relaxed);
            // Woken with `parked` false: pushes until the queues run dry cost no wake. Falls
            // through to the re-test, so a spurious wake re-parks rather than spinning.
            if OBJ_REAP_BATCH_WAKE {
                q = r.work.wait_waiters(q, Some(REAP_BATCH_LINGER), None).0;
                reapstats::LINGERS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    REAPER.call_once(|| Reaper {
        work: CondVar::new(),
        queue: crate::spinlock::Spinlock::new(ReapQueue::new()),
    });
    crate::thread::entry::start_new_kernel(
        crate::thread::priority::Priority::USER,
        reaper_entry,
        0,
        "obj-reaper",
    );
}

/// Ask the reaper to check `obj`, which an unmap may have just made reapable.
///
/// Cheap and non-blocking: a link store and, usually, a signal. The per-object locks the check
/// needs are taken on the reaper thread instead of on the unmap path.
pub fn request_reap(obj: &ObjectRef) {
    use crate::memory::context::virtmem::unmapprofile as up;
    let Some(reaper) = REAPER.poll() else {
        return;
    };
    let t = up::start();
    let (parked, depth) = {
        let mut q = reaper.queue.lock();
        // Already queued means already covered -- see [ReapQueue::objs], which also explains why
        // this test is mandatory rather than an optimization.
        if obj.reap_link.is_linked() {
            reapstats::DEDUPED.fetch_add(1, Ordering::Relaxed);
        } else {
            q.objs.push_back(obj.clone());
            q.depth += 1;
        }
        let (parked, depth) = (q.parked, q.depth);
        // Claim the wake under the same lock that published `parked`, so the next push in this
        // burst suppresses instead of re-signalling a reaper that is already on its way.
        if parked {
            q.parked = false;
        }
        (parked, depth)
    };
    up::record(up::Stage::ReapPush, t);
    // Only when the thread is actually parked, if the knobs say so. `parked` is written under this
    // same lock and `CondVar::wait` registers the waiter before it releases the guard, so a
    // producer that reads `true` is reading a waiter that is already queued -- and one that reads
    // `false` is looking at a thread that has not yet re-tested the queue it just pushed to.
    // Neither can lose the wakeup.
    if should_signal_reaper(parked, depth) {
        let t = up::start();
        reaper.work.signal();
        up::record(up::Stage::ReapSignal, t);
    }
}

/// Wake the reaper now, whatever [`OBJ_REAP_BATCH_WAKE`] would have decided.
///
/// For the memory-wait path. A lingering reaper is sitting on `graves` -- whole object page-table
/// chains, and the frames behind them -- and [`should_signal_reaper`] deliberately trades reap
/// latency for wakes. That is the right trade until someone is about to block for memory, at which
/// point the frames matter more than the 1,896 ns.
///
/// Common-mode across the batching arms rather than gated on the const: with batching off the
/// reaper is never lingering, so this finds no waiter on the condvar and `signal` early-outs on the
/// empty queue.
pub fn poke_reaper() {
    if let Some(reaper) = REAPER.poll() {
        reaper.work.signal();
    }
}

/// A/B knob for the handover below. `false` restores tearing the tables down on whichever thread
/// dropped the last reference, which is the behaviour every measurement before it was taken
/// against -- including the one that says a three-pass sysbench boot exhausts memory.
/// How often the lazily-built cold fields were actually built.
///
/// The question this answers is whether [`OBJ_EAGER_COLD_FIELDS`] can be moving a benchmark at
/// all: eager builds both boxes for **every** object, lazy builds them only for objects something
/// sleeps on or that carry device interrupts. If these counts are small against the boot's
/// `ObjectCreate` count, the lazy arm does strictly less allocator work per object and cannot be
/// losing to the eager one through allocation -- which sends the search to the measurement.
///
/// Counted inside the `call_once` initializer, so it costs one relaxed add per *first* use of a
/// field and nothing on any later one. Printed unconditionally, including zero: "never built" is
/// the informative outcome and a silent counter cannot be told from one that failed to build.
/// Whether the batching mechanism engaged, as opposed to whether a benchmark moved.
///
/// Gate 1, registered before measurement: `suppressed/(sent+suppressed)` should be far above the
/// 4.9% `REAP_SIGNAL_ONLY_WHEN_PARKED` reached on its own, and mean batch should exceed 1. A null
/// on either means the mechanism did not fire, and no bench delta underneath it is interpretable.
///
/// Carries its own positive control: `sent + suppressed` counts every time an unmap of a deleted
/// object reached the decision at all, and that was measured at 65,101 in one boot. A total near
/// zero therefore means the counter was never *reached* -- a broken instrument -- rather than a
/// quiet mechanism. The two readings are distinguishable only because this total is printed, so
/// it is printed including zero.
pub mod reapstats {
    use core::sync::atomic::{AtomicU64, Ordering};

    pub static SIGNALS_SENT: AtomicU64 = AtomicU64::new(0);
    pub static SIGNALS_SUPPRESSED: AtomicU64 = AtomicU64::new(0);
    /// Wakes, i.e. times the thread left its untimed park.
    ///
    /// **Not comparable to the `batches=` this replaced.** That counted drain *iterations*, and
    /// the old drain took the whole queue per iteration while this one takes one entry, so the
    /// two count different events and `mean_batch` moves by redefinition alone. Comparing wake
    /// counts across that change needs the same counter on both sides, which no arm has.
    pub static BATCHES: AtomicU64 = AtomicU64::new(0);
    pub static DRAINED_OBJS: AtomicU64 = AtomicU64::new(0);
    pub static DRAINED_GRAVES: AtomicU64 = AtomicU64::new(0);
    pub static LINGERS: AtomicU64 = AtomicU64::new(0);
    /// Times the reaper parked with a nonzero `depth` and both queues empty -- i.e. the counter
    /// disagreed with the lists. Must be 0. See the park site.
    pub static DEPTH_DRIFT: AtomicU64 = AtomicU64::new(0);
    /// Pushes skipped because the object was already queued. New with the intrusive queue: this
    /// is the work the old duplicate-carrying `Vec` did and this one does not, and it is the
    /// mechanism gate for the dedupe -- a zero here means dedupe never engaged.
    pub static DEDUPED: AtomicU64 = AtomicU64::new(0);
    /// Reaps admitted through the stale-map-count escape hatch (`is_reapable`): a positive
    /// count with no live mapping. Nonzero means some teardown path swallowed a dec.
    pub static STALE_COUNT_REAPS: AtomicU64 = AtomicU64::new(0);

    pub fn print() {
        let sent = SIGNALS_SENT.load(Ordering::Relaxed);
        let sup = SIGNALS_SUPPRESSED.load(Ordering::Relaxed);
        let batches = BATCHES.load(Ordering::Relaxed);
        let objs = DRAINED_OBJS.load(Ordering::Relaxed);
        let graves = DRAINED_GRAVES.load(Ordering::Relaxed);
        let total = sent + sup;
        // Integer math only; mean batch scaled by 1000 rather than a float.
        logln!(
            "== reaper wake: sent={} suppressed={} ({}% of {}) wakes={} objs={} graves={} deduped={} lingers={} depth_drift={} mean_batch_x1000={} stale_count_reaps={} (batch_wake={} defer_teardown={} targeted={} mapcount_fast={}) ==",
            sent,
            sup,
            if total > 0 { sup * 100 / total } else { 0 },
            total,
            batches,
            objs,
            graves,
            DEDUPED.load(Ordering::Relaxed),
            LINGERS.load(Ordering::Relaxed),
            DEPTH_DRIFT.load(Ordering::Relaxed),
            if batches > 0 {
                (objs + graves) * 1000 / batches
            } else {
                0
            },
            STALE_COUNT_REAPS.load(Ordering::Relaxed),
            // Every const that governs this subsystem's behaviour, emitted with the numbers it
            // governs. A handover message describing tree state can be wrong or absent -- this
            // rides in the artifact, so a transcript is self-identifying about the configuration
            // it was produced under. `DEFER_TEARDOWN` especially: see its doc.
            super::OBJ_REAP_BATCH_WAKE,
            super::DEFER_TEARDOWN,
            super::TARGETED_REAP,
            super::OBJ_REAP_MAP_COUNT_FAST,
        );
    }
}

pub mod coldfieldstats {
    use core::sync::atomic::{AtomicU64, Ordering};

    pub static SLEEP_INITS: AtomicU64 = AtomicU64::new(0);
    pub static DEV_INITS: AtomicU64 = AtomicU64::new(0);

    pub fn print() {
        logln!(
            "== object cold fields built lazily: {} sleep tables, {} device-interrupt tables (eager: {}) ==",
            SLEEP_INITS.load(Ordering::Relaxed),
            DEV_INITS.load(Ordering::Relaxed),
            super::OBJ_EAGER_COLD_FIELDS,
        );
    }
}

/// A/B knob for the lazily-built cold fields on [`Object`] (`sleep_slot`,
/// `device_interrupt_info`). `true` builds both at create, which is what every measurement before
/// this change was taken against; the struct is the small one either way, so this isolates the
/// allocation from the shrink rather than restoring the old layout.
pub const OBJ_EAGER_COLD_FIELDS: bool = true;

/// A/B knob for skipping the reaper wake when the reaper is not parked.
///
/// Every unmap of a deleted object signals the reaper, and `CondVar::signal` costs a critical
/// section, a spinlock and a `requeue_all` even when it wakes nobody. Measured at 2,147 ns per
/// unmap inside `remove_object`'s `finish` stage -- 9% of `object_create_delete`.
///
/// Measured **inert on its own**: it skipped 3,197 of 65,101 signals (4.9%), because the reaper
/// drains its whole queue and re-parks between consecutive unmaps, so essentially every push
/// finds it parked. Superseded by [`OBJ_REAP_BATCH_WAKE`], which is what makes "not parked" a
/// state that lasts long enough to be worth testing; this const only selects the old behaviour
/// when batching is off.
pub const REAP_SIGNAL_ONLY_WHEN_PARKED: bool = false;

/// A/B knob for amortizing the reaper wake across a batch of objects.
///
/// The wake, not the reaping, is what an unmap pays: `request_reap` is a push and a
/// `CondVar::signal`, and that signal is a topology walk (`select_cpu`), an insert into a *remote*
/// run queue, and an IPI -- 1,896 ns, against 2,606 ns for the reap itself on the reaper thread.
///
/// So the reaper lingers instead of parking the instant it is woken. During the linger `parked` is
/// false, and a pusher that sees that skips the signal entirely: the reaper is going to re-test
/// the queue before it sleeps again, and the same-lock argument on [`ReapQueue::parked`] says it
/// cannot miss what was pushed. One wake per batch rather than one per unmap.
///
/// Two bounds, because a queued object holds frames: [`REAP_BATCH_LINGER`] caps how long a batch
/// accumulates, and [`REAP_BATCH_MAX`] cuts the linger short when enough has piled up that the
/// memory matters more than the wake. Nothing can strand -- a push that finds the reaper genuinely
/// parked always signals.
pub const OBJ_REAP_BATCH_WAKE: bool = true;

/// How long the reaper accumulates before draining. Bounds reap latency, and with it how long a
/// deleted object's frames stay out of circulation.
///
/// Not a busy-wait: it is a timed condvar wait, so the cpu is free and a
/// [`REAP_BATCH_MAX`]-sized batch still cuts it short. The one caveat is that it rides the kernel
/// timeout queue, which only the bsp advances -- a bsp spinning with interrupts off delays this
/// like every other timeout -- which is survivable for a cleanup thread and would not be for
/// anything on a syscall path.
pub const REAP_BATCH_LINGER: core::time::Duration = core::time::Duration::from_micros(500);

/// Batch depth at which a push wakes the reaper early rather than waiting out
/// [`REAP_BATCH_LINGER`]. Counts queued objects and graves together, since both hold frames.
pub const REAP_BATCH_MAX: usize = 64;

/// Whether a push of depth `depth` that found the reaper `parked` must wake it.
///
/// `parked` is the load-bearing one: it means the reaper is in an *untimed* wait, so nothing else
/// will ever look at the queue and skipping the signal would strand the work. Every other state --
/// working, or lingering -- ends in a re-test of the queue under the lock this was pushed under.
///
/// Callers clear `parked` *at the moment they decide to signal* rather than leaving it for the
/// reaper to clear when it runs. Measured: without that, `parked` stays true for the whole wake
/// latency -- the scheduler insert and the IPI this change exists to avoid -- and every push
/// arriving in that window pays a full `CondVar::signal` that wakes nobody, because the reaper is
/// already off the condvar queue. On smp1, where the reaper cannot run at all until the unmapper
/// yields, that was 1,762 signals for 178 wakes (11% suppressed); on smp4 the reaper clears it
/// from another cpu and the same boot suppressed 75%. The batches were ~11 objects either way, so
/// the draining was always batched -- it was only the signalling that was not.
fn should_signal_reaper(parked: bool, depth: usize) -> bool {
    let signal = if OBJ_REAP_BATCH_WAKE {
        parked || depth >= REAP_BATCH_MAX
    } else {
        !REAP_SIGNAL_ONLY_WHEN_PARKED || parked
    };
    // Counted here rather than at the two call sites, so the total is the number of decisions
    // rather than the number of places that make them.
    if signal {
        reapstats::SIGNALS_SENT.fetch_add(1, Ordering::Relaxed);
    } else {
        reapstats::SIGNALS_SUPPRESSED.fetch_add(1, Ordering::Relaxed);
    }
    signal
}

/// Flipping this to `false` is not just a perf arm. `ObjectPageTable::drop` frees frames with a
/// `WAIT_OK` allocator, so it can block for memory; deferred, that runs on the reaper with no lock
/// held. Inline, it runs on whatever thread released the last reference -- which may hold a
/// spinlock, and blocking with a spinlock held has no check anywhere in this kernel
/// (`DISABLE_LOCK_TRACKING` is hardcoded on, and `Spinlock::lock` never touches
/// `critical_counter`).
pub const DEFER_TEARDOWN: bool = true;

/// Hand a dead object's page tables to the reaper to tear down. Never blocks and never allocates:
/// a link store and a signal.
fn defer_teardown(home: Box<PtHome>) {
    if !DEFER_TEARDOWN {
        drop(home);
        return;
    }
    let Some(reaper) = REAPER.poll() else {
        // Before the reaper thread exists nothing else can free these, so the caller pays. Early
        // boot only, and the objects that die there are the bootstrap ones -- small, and dropped
        // by the thread that built them rather than by a service thread.
        drop(home);
        return;
    };
    let (parked, depth) = {
        let mut q = reaper.queue.lock();
        q.graves.push_back(home);
        q.depth += 1;
        let (parked, depth) = (q.parked, q.depth);
        // See `should_signal_reaper`: claim the wake here, not when the reaper next runs.
        if parked {
            q.parked = false;
        }
        (parked, depth)
    };
    if should_signal_reaper(parked, depth) {
        reaper.work.signal();
    }
}

/// Try to reap one object that has just been marked for deletion.
///
/// What [`ObjectControlCmd::Delete`] wants: the object it named is the only one whose reapability
/// just changed, and a full [`scan_deleted`] to catch it walks the entire global object map under
/// its lock -- which is most of what a delete syscall cost (18.6 us of a 34.9 us create/delete
/// pair, `sysbench.md` F6). Objects that become reapable *later*, because someone else's mapping
/// went away, are still caught by the idle-loop scan.
pub fn scan_deleted_one(obj: &ObjectRef) {
    if !obj.is_pending_delete() || !is_reapable(obj) {
        return;
    }
    use crate::syscall::object::deleteprofile;
    let t = deleteprofile::start();
    reap_one(obj);
    deleteprofile::record(deleteprofile::Stage::Reap, t);
}

/// Returns the number of objects reaped, so an explicit sweep
/// ([`twizzler_abi::syscall::SysCtrlCmd::ReapAll`]) can tell "nothing was pending" from "I did
/// work". The idle-loop and unmap callers ignore it.
pub fn scan_deleted() -> usize {
    // Never take a per-object lock while holding the global map lock. An object's page-table lock
    // can be held by a thread that is asleep waiting on the userspace pager, and blocking on it
    // here would stall every lookup_object() in the kernel -- including the ones the pager request
    // handler needs to service that very wait. So: pick candidates using only the cheap test,
    // release the map lock, then evaluate the blocking predicate.
    let candidates = if OMAP_SHARDED {
        let mut v = Vec::new();
        obj_manager().sharded.collect_pending(&mut v);
        v
    } else {
        let om = obj_manager().map.lock();
        om.iter()
            .filter(|(_, obj)| obj.is_pending_delete())
            .map(|(id, obj)| (*id, obj.clone()))
            .collect::<Vec<_>>()
    };

    let mut reaped = 0;
    for (_, obj) in candidates {
        if is_reapable(&obj) {
            reap_one(&obj);
            reaped += 1;
        }
    }
    reaped
}

/// Ring of the most recent `mark_for_delete` calls, so that a failed lookup can say whether the id
/// it was handed used to exist and who killed it. Diagnostic only.
///
/// Lock-free on purpose: this is written from every `mark_for_delete`, i.e. from the teardown path
/// the races under investigation run through, so a lock here would serialize exactly what it is
/// meant to observe. Torn or stale reads only ever cost a missed report.
struct DeleteSlot {
    /// Low 64 bits of the id; ids are random, so this is enough to identify one. 0 means empty.
    id_lo: AtomicU64,
    loc: core::sync::atomic::AtomicPtr<core::panic::Location<'static>>,
}

impl DeleteSlot {
    const fn new() -> Self {
        Self {
            id_lo: AtomicU64::new(0),
            loc: core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()),
        }
    }
}

const DELETE_RING_LEN: usize = 64;
static DELETE_RING: [DeleteSlot; DELETE_RING_LEN] = [const { DeleteSlot::new() }; DELETE_RING_LEN];
static DELETE_SEQ: AtomicU64 = AtomicU64::new(0);

fn record_delete(id: ObjID, loc: &'static core::panic::Location<'static>) {
    let idx = (DELETE_SEQ.fetch_add(1, Ordering::Relaxed) as usize) % DELETE_RING_LEN;
    let slot = &DELETE_RING[idx];
    // Invalidate before overwriting, so a reader never pairs a new id with an old location.
    slot.id_lo.store(0, Ordering::Relaxed);
    slot.loc.store(loc as *const _ as *mut _, Ordering::Relaxed);
    slot.id_lo.store(id.raw() as u64, Ordering::Release);
}

/// If this id was marked for delete recently, report how many marks ago and from where.
fn lookup_delete(id: ObjID) -> Option<(usize, &'static core::panic::Location<'static>)> {
    let want = id.raw() as u64;
    if want == 0 {
        return None;
    }
    let seq = DELETE_SEQ.load(Ordering::Relaxed) as usize;
    for back in 1..=DELETE_RING_LEN {
        let slot = &DELETE_RING[seq.wrapping_sub(back) % DELETE_RING_LEN];
        if slot.id_lo.load(Ordering::Acquire) == want {
            let loc = slot.loc.load(Ordering::Relaxed);
            if !loc.is_null() {
                // Safety: only ever stores &'static Location values.
                return Some((back, unsafe { &*loc }));
            }
        }
    }
    None
}

/// Describe why a lookup of `id` may have failed, for diagnostics on the miss path.
pub fn describe_missing(id: ObjID) -> DeleteInfo {
    DeleteInfo {
        no_exist: is_no_exist(id),
        deleted: lookup_delete(id),
    }
}

pub struct DeleteInfo {
    no_exist: bool,
    deleted: Option<(usize, &'static core::panic::Location<'static>)>,
}

impl Display for DeleteInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.no_exist {
            write!(f, "marked non-existent by the pager; ")?;
        }
        match self.deleted {
            Some((back, loc)) => write!(f, "marked for delete {} deletes ago, from {}", back, loc),
            None => write!(f, "not in the recent delete ring"),
        }
    }
}

static OBJ_MANAGER: Once<ObjectManager> = Once::new();

fn obj_manager() -> &'static ObjectManager {
    OBJ_MANAGER.call_once(|| ObjectManager::new())
}

pub fn lookup_object(id: ObjID, flags: LookupFlags) -> LookupResult {
    obj_manager().lookup_object(id, flags)
}

pub fn register_object(obj: Arc<Object>) {
    ties::TIE_MGR.create_object_ties(obj.id(), obj.ties.iter().map(|tie| tie.id.into()));
    obj_manager().register_object(obj);
}

pub fn no_exist(id: ObjID) {
    obj_manager().no_exist.lock().insert(id);
}

pub fn is_no_exist(id: ObjID) -> bool {
    obj_manager().no_exist.lock().contains(&id)
}

/// Forget a negative-cache entry. Nothing else removes one, so an id marked nonexistent stays that
/// way for the boot; only a create of that exact id knows the answer has changed.
pub fn clear_no_exist(id: ObjID) {
    obj_manager().no_exist.lock().remove(&id);
}

/// Report what large-page promotion would win, if the picture has changed since the last report.
///
/// Sizing the prize before building it (`largepager.md`): promotion is real mapper work with a
/// shootdown, and it is only worth it if regions actually end up fully populated with contiguous
/// aligned 4 KiB frames. This is a scan rather than a hot-path counter because the question is
/// about *state* -- which regions ended up that way -- and a scan cannot double-count a region that
/// is touched again later, which an event counter would.
///
/// Diagnostic, and not free: called from the idle loop's test/diag block, throttled, and quiet
/// unless a number moves.
/// Composition of object-held pages, printed from the allocator's wait path under memory
/// pressure. Answers the reclaim-design question pagerwedge.md §3.7 leaves open: how much of
/// the "page" share is backed (evictable to disk) vs volatile (not evictable without swap),
/// and whether it is concentrated or diffuse. Allocation-free ([omap::ShardedOmap::
/// for_each_chunked]) because it runs while allocation is failing; `count_pages` is O(1) with
/// the mapper's exact counter.
pub fn pressure_census() {
    if !OMAP_SHARDED {
        return;
    }
    let mut backed = 0usize;
    let mut backed_objs = 0usize;
    let mut vol = 0usize;
    let mut vol_objs = 0usize;
    let mut del = 0usize;
    // (pages, id, backed, deleted, arc strong count, live mappings) -- refs and mappings name
    // the holder class: refs with no mappings point at kernel-side owners (unreaped threads,
    // reaper graves); live mappings point at userspace still holding the object mapped
    // (monitor unmapper backlog, runtime handle caches).
    let mut top: heapless::Vec<(usize, ObjID, bool, bool, usize, usize), 8> = heapless::Vec::new();
    // Name the owner of the first few pending-delete objects' still-live mappings, not just
    // count them: `removed=true` with the weak still upgradeable means the region left its
    // RegionManager but an Arc clone survives (a deferred-unmap queue somewhere);
    // `removed=false` means nothing ever unmapped it.
    let mut detail_budget = 4usize;
    // Aggregate split over the WHOLE pending-delete population -- the 6-sample detail above is
    // omap-order biased and kept landing on live compartments' legit anonymous objects
    // (reclaim10/11). Regions classify by target_sctx: 0 (never swept by anything), live (in
    // the sctx registry -- a live compartment's own anonymous object, not a leak), or dead
    // (nonzero, unregistered -- the sweep should have taken it).
    let live_sctxs = crate::security::sctx_registry_ids();
    let mut pd_mapped_objs = 0usize;
    let mut pd_mapped_pages = 0usize;
    let mut pd_unmapped_objs = 0usize;
    let mut pd_unmapped_pages = 0usize;
    let mut r_sctx0 = 0usize;
    let mut r_live = 0usize;
    let mut r_dead = 0usize;
    let mut pg_sctx0 = 0usize;
    let mut pg_live = 0usize;
    let mut pg_dead = 0usize;
    let mut pd_stuck_mapcount = 0usize;
    let mut pg_stuck_mapcount = 0usize;
    let mut mc_sum = 0usize;
    let mut stuck_budget = 3usize;
    let mut pd_stuck_pins = 0usize;
    let mut pd_stuck_linked = 0usize;
    let mut pd_stuck_none = 0usize;
    let mut pg_stuck_none = 0usize;
    obj_manager().sharded.for_each_chunked(|obj| {
        let pages = obj.lock_page_tables().count_pages();
        if obj.is_pending_delete() {
            del += pages;
        }
        if obj.use_pager() {
            backed += pages;
            backed_objs += 1;
        } else {
            vol += pages;
            vol_objs += 1;
        }
        // Upgradeable only: `len()` counts dead weaks, and a corpse posing as a live mapping
        // is exactly the ambiguity this census exists to remove.
        let mut live_maps = 0usize;
        if obj.is_pending_delete() || !top.is_full() || pages > top.last().map(|t| t.0).unwrap_or(0)
        {
            let maps = obj.mappings.lock();
            for (slot, weak) in maps.iter() {
                let Some(region) = weak.upgrade() else {
                    continue;
                };
                live_maps += 1;
                if obj.is_pending_delete() {
                    let sctx = region.target_sctx;
                    let interesting = if sctx.raw() == 0 {
                        r_sctx0 += 1;
                        pg_sctx0 += pages;
                        true
                    } else if live_sctxs.contains(&sctx) {
                        r_live += 1;
                        pg_live += pages;
                        false
                    } else {
                        r_dead += 1;
                        pg_dead += pages;
                        true
                    };
                    // Sample only the leak-candidate classes (sctx0/dead) -- live-compartment
                    // regions are legit anonymous objects and drowned the old sampling.
                    if interesting && detail_budget > 0 {
                        detail_budget -= 1;
                        logln!(
                            "  del-map: obj {} pages {} slot {} sctx {} prot {:?} removed {} va {:x}",
                            obj.id(),
                            pages,
                            slot,
                            sctx,
                            region.prot,
                            region.removed.load(Ordering::Relaxed),
                            region.range.start.raw()
                        );
                    }
                }
            }
            if obj.is_pending_delete() {
                if live_maps > 0 {
                    pd_mapped_objs += 1;
                    pd_mapped_pages += pages;
                } else {
                    pd_unmapped_objs += 1;
                    pd_unmapped_pages += pages;
                    // Why is this region-less pending-delete object not reaped? Exactly one of
                    // these should explain each: a stuck map count (accounting leak --
                    // `is_reapable` false forever), a pin, a reap request parked in the queue
                    // (linked), or none of the above (request lost / never made).
                    if obj.map_count() != 0 {
                        pd_stuck_mapcount += 1;
                        pg_stuck_mapcount += pages;
                        // Sum says whether the phantom count is exactly 1 per object (one
                        // specific install site leaks) or varies; the note names the class.
                        mc_sum += obj.map_count();
                        if stuck_budget > 0 {
                            stuck_budget -= 1;
                            let mut nbuf = [0u8; 64];
                            let n = obj.vnotes.summarize(&mut nbuf);
                            logln!(
                                "  pd-stuck: obj {} pages {} mapcount {} note '{}'",
                                obj.id(),
                                pages,
                                obj.map_count(),
                                core::str::from_utf8(&nbuf[..n]).unwrap_or("?")
                            );
                        }
                    } else if obj.pin_info.lock().pins.len() != 0 {
                        pd_stuck_pins += 1;
                    } else if obj.reap_link.is_linked() {
                        pd_stuck_linked += 1;
                    } else {
                        pd_stuck_none += 1;
                        pg_stuck_none += pages;
                    }
                }
            }
        }
        if !top.is_full() || pages > top.last().map(|t| t.0).unwrap_or(0) {
            if top.is_full() {
                top.pop();
            }
            let refs = alloc::sync::Arc::strong_count(obj);
            let _ = top.push((
                pages,
                obj.id(),
                obj.use_pager(),
                obj.is_pending_delete(),
                refs,
                live_maps,
            ));
            top.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        }
    });
    logln!(
        "PRESSURE-CENSUS: backed {} pages / {} objs; volatile {} pages / {} objs; pending-delete {} pages; exited-backlog {}; sctx del {} drop {}",
        backed,
        backed_objs,
        vol,
        vol_objs,
        del,
        crate::processor::EXITED_BACKLOG.load(core::sync::atomic::Ordering::Relaxed),
        crate::security::SCTX_DELETES.load(core::sync::atomic::Ordering::Relaxed),
        crate::security::SCTX_DROPS.load(core::sync::atomic::Ordering::Relaxed)
    );
    logln!(
        "PD-SPLIT: mapped {} objs / {} pages, unmapped {} objs / {} pages | regions sctx0 {} ({} pg) live {} ({} pg) dead {} ({} pg)",
        pd_mapped_objs,
        pd_mapped_pages,
        pd_unmapped_objs,
        pd_unmapped_pages,
        r_sctx0,
        pg_sctx0,
        r_live,
        pg_live,
        r_dead,
        pg_dead
    );
    logln!(
        "PD-STUCK: mapcount {} ({} pg, mc_sum {}) pins {} linked {} none {} ({} pg)",
        pd_stuck_mapcount,
        pg_stuck_mapcount,
        mc_sum,
        pd_stuck_pins,
        pd_stuck_linked,
        pd_stuck_none,
        pg_stuck_none
    );
    logln!(
        "PRESSURE-CENSUS2: sctx registry {}; threads new {} dropped {}",
        crate::security::sctx_registry_len(),
        crate::thread::THREAD_NEWS.load(core::sync::atomic::Ordering::Relaxed),
        crate::thread::THREAD_DROPS.load(core::sync::atomic::Ordering::Relaxed)
    );
    for (pages, id, b, d, refs, maps) in &top {
        logln!(
            "  top: {} pages obj {} {}{} refs {} maps {}",
            pages,
            id,
            if *b { "backed" } else { "volatile" },
            if *d { " DELETED" } else { "" },
            refs,
            maps
        );
    }
}

pub fn promotion_census() {
    static TICK: AtomicU64 = AtomicU64::new(0);
    static LAST: Mutex<Option<pagetables::PromotionCensus>> = Mutex::new(None);
    if !TICK.fetch_add(1, Ordering::Relaxed).is_multiple_of(16) {
        return;
    }

    // Never take a per-object lock while holding the global map lock -- see `scan_deleted` for what
    // that deadlocks against.
    let objects = obj_manager()
        .map
        .lock()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut census = pagetables::PromotionCensus::default();
    for obj in objects {
        obj.lock_page_tables().promotion_census(&mut census);
    }

    let mut last = LAST.lock();
    if *last == Some(census) {
        return;
    }
    *last = Some(census);
    // Reported against the allocator's own used-page count, because the region counts are only
    // believable if the memory they claim exists: a census asserting more pages mapped into objects
    // than the machine has allocated is measuring something other than what it says.
    let mut mem = twizzler_abi::syscall::MemoryStats::default();
    crate::memory::frame::fill_stats(&mut mem);
    let free = mem
        .levels()
        .iter()
        .map(|level| level.free_pages * (level.page_size / FRAME_SIZE))
        .sum::<usize>();
    log::info!(
        "PROMOTE: {} objects; regions: {} large, {} promotable, {} shared, {} full-but-unaligned, {} partial ({} pages), {} region-0; {} pages in objects, {} of {} used",
        census.objects,
        census.large,
        census.promotable,
        census.shared,
        census.unaligned,
        census.partial,
        census.partial_pages,
        census.region0,
        census.pages(),
        mem.total_pages.saturating_sub(free),
        mem.total_pages,
    );
}

pub fn get_object_stats() -> twizzler_abi::syscall::ObjectStats {
    // Behind `--diag`, because this is the *stats syscall* and it was dumping every object in the
    // system to the console on every call. `leakcheck`'s sampler calls `sys_object_stats` once per
    // iteration, so a 220-iteration op emitted ~800 console lines per sample: 115,831 ObjectInfo
    // lines and 17 MB of serial from one op, which timed out two runs and perturbs every
    // measurement that samples object stats. Gated rather than deleted -- this file is another
    // session's uncommitted work and the dump is clearly wanted sometimes; `--diag` is where the
    // kernel's other walk-everything diagnostics already live.
    if crate::is_diag_mode() {
        print_all_objects();
    }
    let mut stats = twizzler_abi::syscall::ObjectStats::default();
    if OMAP_SHARDED {
        // Count from a snapshot taken outside the shard locks: `is_mapped` takes the object's
        // sleeping page-table mutex, which must not run under a shard spinlock.
        let mut objs = Vec::new();
        obj_manager().sharded.collect_all(&mut objs);
        stats.nr_objects = objs.len();
        stats.nr_mapped = objs.iter().filter(|obj| obj.is_mapped()).count();
    } else {
        let mgr = obj_manager().map.lock();
        stats.nr_objects = mgr.len();
        stats.nr_mapped = mgr.values().filter(|obj| obj.is_mapped()).count();
    }
    stats.nr_handles = count_handles();
    ties::fill_stats(&mut stats);

    stats
}

pub fn enumerate_objects(buf: &mut [ObjID], offset: usize) -> Result<usize, TwzError> {
    let ids = if OMAP_SHARDED {
        // Shard-major snapshot union; enumerate was never transactional, so this is the same
        // guarantee class as the single-lock walk.
        let mut all = Vec::new();
        obj_manager().sharded.collect_ids(&mut all);
        TIE_MGR.with_deleted_map(|dm| all.extend(dm.keys().copied()));
        all.into_iter()
            .skip(offset)
            .take(buf.len())
            .collect::<Vec<_>>()
    } else {
        let mgr = obj_manager().map.lock();
        TIE_MGR.with_deleted_map(|dm| {
            mgr.iter()
                .chain(dm.iter())
                .map(|(id, _)| *id)
                .skip(offset)
                .take(buf.len())
                .collect::<Vec<_>>()
        })
    };

    buf[..ids.len()].copy_from_slice(&ids);
    Ok(ids.len())
}
