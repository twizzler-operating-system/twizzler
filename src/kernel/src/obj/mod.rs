use alloc::{
    boxed::Box,
    collections::{BTreeMap, btree_set::BTreeSet},
    sync::Arc,
    vec::Vec,
};
use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
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
    idcounter::{IdCounter, SimpleId},
    memory::{
        VirtAddr,
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
pub struct Object {
    pub id: ObjID,
    flags: AtomicU32,
    tables: Mutex<pagetables::ObjectPageTable>,
    sleep_info: Mutex<SleepInfo>,
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
    device_interrupt_info: Box<[(AtomicU64, AtomicU64); NUM_DEVICE_INTERRUPTS]>,
    pin_info: Mutex<PinInfo>,
    lifetime_type: LifetimeType,
    ties: Vec<object_tie>,
    verified_id: OnceWait<(bool, Protections)>,
    /// The backing store's data length. `u64::MAX` means "never told"; see [Object::known_len].
    known_len: AtomicU64,
    vnotes: VNotes,
}

impl Drop for Object {
    fn drop(&mut self) {
        if self.use_pager() && self.is_pending_delete() {
            crate::pager::del_object(self.id);
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
        self.lock_page_tables().map_count() > 0
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
        PtGuard::new(&self.tables)
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
        Self {
            id,
            flags: AtomicU32::new(0),
            tables: Mutex::new(pagetables::ObjectPageTable::new()),
            sleep_info: Mutex::new(SleepInfo::new(id)),
            sleepers: AtomicUsize::new(0),
            pin_info: Mutex::new(PinInfo::default()),
            ties: ties.to_vec(),
            verified_id: OnceWait::new(),
            known_len: AtomicU64::new(u64::MAX),
            lifetime_type,
            device_interrupt_info: Box::new(
                [const { (AtomicU64::new(0), AtomicU64::new(0)) }; NUM_DEVICE_INTERRUPTS],
            ),
            vnotes: VNotes::new(),
        }
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
    fn init_meta(self: &Arc<Self>, meta: MetaInfo) {
        let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK);
        // Safety: a freshly allocated frame, named by nothing else, and `MetaInfo` sits at offset
        // zero of the meta page -- the offset `write_meta` writes it to.
        unsafe { frame.virtaddr().as_mut_ptr::<MetaInfo>().write(meta) };
        self.add_frame(PageNumber::meta_page(), frame);
        self.note_written_meta(&meta);
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
            (page_tree.count_pages(), page_tree.map_count())
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

struct ObjectManager {
    map: Mutex<BTreeMap<ObjID, ObjectRef>>,
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
            no_exist: Mutex::new(BTreeSet::new()),
        }
    }

    fn lookup_object(&self, id: ObjID, _flags: LookupFlags) -> LookupResult {
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
        ties::TIE_MGR
            .lookup_object(id)
            .map_or(LookupResult::NotFound, |obj| LookupResult::Found(obj))
    }

    fn register_object(&self, obj: Arc<Object>) {
        // TODO: what if it returns an obj
        self.map.lock().insert(obj.id(), obj);
    }
}

pub fn print_all_objects() {
    let mgr = obj_manager();
    let map = mgr.map.lock();
    logln!("=== OBJECTS === ({})", map.len());
    let mut nn = 0;
    for (id, obj) in map.iter() {
        logln!("{}: {:?}", id, obj.info());
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

pub fn scan_deleted() {
    // Never take a per-object lock while holding the global map lock. An object's page-table lock
    // can be held by a thread that is asleep waiting on the userspace pager, and blocking on it
    // here would stall every lookup_object() in the kernel -- including the ones the pager request
    // handler needs to service that very wait. So: pick candidates using only the cheap test,
    // release the map lock, then evaluate the blocking predicate.
    let candidates = {
        let om = obj_manager().map.lock();
        om.iter()
            .filter(|(_, obj)| obj.is_pending_delete())
            .map(|(id, obj)| (*id, obj.clone()))
            .collect::<Vec<_>>()
    };

    let mut deletable = Vec::new();
    for (id, obj) in candidates {
        let not_mapped = obj.lock_page_tables().map_count() == 0;
        if not_mapped && obj.pin_info.lock().pins.len() == 0 {
            deletable.push((id, obj));
        }
    }

    let dobjs = {
        let mut om = obj_manager().map.lock();
        let mut dobjs = Vec::new();
        for (id, obj) in deletable {
            // Re-check under the map lock: the entry may have been replaced or resurrected while
            // we were evaluating it unlocked.
            let unchanged = om
                .get(&id)
                .is_some_and(|cur| Arc::ptr_eq(cur, &obj) && cur.is_pending_delete());
            if unchanged {
                om.remove(&id);
                dobjs.push(obj);
            }
        }
        dobjs
    };

    for dobj in dobjs {
        ties::TIE_MGR.delete_object(dobj);
    }
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
    print_all_objects();
    let mut stats = twizzler_abi::syscall::ObjectStats::default();
    let mgr = obj_manager().map.lock();
    stats.nr_objects = mgr.len();
    stats.nr_mapped = mgr.values().filter(|obj| obj.is_mapped()).count();
    stats.nr_handles = count_handles();
    ties::fill_stats(&mut stats);

    stats
}

pub fn enumerate_objects(buf: &mut [ObjID], offset: usize) -> Result<usize, TwzError> {
    let mgr = obj_manager().map.lock();
    let ids = TIE_MGR.with_deleted_map(|dm| {
        mgr.iter()
            .chain(dm.iter())
            .map(|(id, _)| *id)
            .skip(offset)
            .take(buf.len())
            .collect::<Vec<_>>()
    });

    buf[..ids.len()].copy_from_slice(&ids);
    Ok(ids.len())
}
