use alloc::{
    collections::{btree_map::Entry, btree_set::BTreeSet, BTreeMap},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, Ordering},
};

use pages::PageRef;
use range::{GetPageFlags, PageStatus};
use twizzler_abi::{
    meta::{MetaFlags, MetaInfo},
    object::{ObjID, Protections, MAX_SIZE},
    syscall::{CreateTieSpec, LifetimeType},
};
use twizzler_rt_abi::object::Nonce;

use self::{pages::Page, thread_sync::SleepInfo};
use crate::{
    arch::memory::frame::FRAME_SIZE,
    idcounter::{IdCounter, SimpleId, StableId},
    memory::{
        context::{kernel_context, Context, ContextRef, UserContext},
        tracker::{alloc_frame, FrameAllocFlags, FrameAllocator},
        PhysAddr, VirtAddr,
    },
    mutex::{LockGuard, Mutex},
    once::{Once, OnceWait},
    random::getrandom,
};

pub mod control;
pub mod copy;
pub mod id;
pub mod pages;
pub mod pagevec;
pub mod range;
pub mod thread_sync;
pub mod ties;

const OBJ_DELETED: u32 = 1;
pub struct Object {
    id: ObjID,
    flags: AtomicU32,
    range_tree: Mutex<range::PageRangeTree>,
    sleep_info: Mutex<SleepInfo>,
    pin_info: Mutex<PinInfo>,
    contexts: Mutex<ContextInfo>,
    lifetime_type: LifetimeType,
    ties: Vec<CreateTieSpec>,
    verified_id: OnceWait<(bool, Protections)>,
}

#[derive(Default)]
struct ContextInfo {
    contexts: BTreeMap<u64, (Weak<Context>, usize)>,
}

impl ContextInfo {
    fn insert(&mut self, ctx: &ContextRef) {
        let entry = self
            .contexts
            .entry(ctx.id().value())
            .or_insert_with(|| (Arc::downgrade(ctx), 0));
        entry.1 += 1;
    }

    fn remove(&mut self, ctx: u64) {
        if let Entry::Occupied(mut x) = self.contexts.entry(ctx) {
            x.get_mut().1 -= 1;
            if x.get().1 == 0 {
                x.remove();
            }
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

    pub fn use_pager(&self) -> bool {
        self.lifetime_type == LifetimeType::Persistent
    }

    pub fn is_kernel_id(&self) -> bool {
        self.id.parts()[0] == 1
    }

    pub fn mark_for_delete(&self) {
        self.flags.fetch_or(OBJ_DELETED, Ordering::SeqCst);
    }

    pub fn lock_page_tree(&self) -> LockGuard<'_, range::PageRangeTree> {
        self.range_tree.lock()
    }

    pub fn add_page(&self, pn: PageNumber, page: PageRef, allocator: Option<&mut FrameAllocator>) {
        let mut range_tree = self.range_tree.lock();
        range_tree.add_page(pn, page, allocator);
    }

    pub fn id(&self) -> ObjID {
        self.id
    }

    pub fn release_pin(&self, _pin: u32) {
        // TODO: Currently we don't track pins. This will be changed in-future when we fully
        // implement eviction.
    }

    pub fn pin(&self, start: PageNumber, len: usize) -> Option<(Vec<PhysAddr>, u32)> {
        assert!(!self.use_pager());
        let mut tree = self.lock_page_tree();

        let mut pin_info = self.pin_info.lock();

        let mut v = Vec::new();
        for i in 0..len {
            // TODO: we'll need to handle failures here when we expand the paging system.
            let p = tree.get_page(start.offset(i), GetPageFlags::empty(), None);
            if let PageStatus::Ready(p, _) = p {
                v.push(p.physical_address());
            } else {
                let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK);
                let page = Page::new(frame);
                v.push(page.physical_address());
                let page = PageRef::new(Arc::new(page), 0, 1);
                tree.add_page(start.offset(i), page, None);
            }
        }

        let id = pin_info.id_counter.next_simple();
        let token = id.value().try_into().ok()?;
        pin_info.pins.push(id);

        Some((v, token))
    }

    pub fn new(id: ObjID, lifetime_type: LifetimeType, ties: &[CreateTieSpec]) -> Self {
        Self {
            id,
            flags: AtomicU32::new(0),
            range_tree: Mutex::new(range::PageRangeTree::new()),
            sleep_info: Mutex::new(SleepInfo::new()),
            pin_info: Mutex::new(PinInfo::default()),
            contexts: Mutex::new(ContextInfo::default()),
            ties: ties.to_vec(),
            verified_id: OnceWait::new(),
            lifetime_type,
        }
    }

    pub fn new_kernel() -> Self {
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
            let obj = Self::new(id::backup_id_gen(), LifetimeType::Volatile, &[]);
            while !obj.write_meta(meta, true) {
                logln!("failed to write object metadata -- retrying");
            }
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
        while !obj.write_meta(meta, true) {
            logln!("failed to write object metadata -- retrying");
        }
        obj
    }

    pub fn add_context(&self, ctx: &ContextRef) {
        self.contexts.lock().insert(ctx)
    }

    pub fn remove_context(&self, id: u64) {
        self.contexts.lock().remove(id)
    }

    pub fn invalidate(&self, range: core::ops::Range<PageNumber>, mode: InvalidateMode) {
        let contexts = self.contexts.lock();
        for ctx in contexts.contexts.values() {
            if let Some(ctx) = ctx.0.upgrade() {
                ctx.invalidate_object(self.id(), &range, mode);
            }
        }
        kernel_context().invalidate_object(self.id(), &range, mode);
    }

    pub fn print_page_tree(&self) {
        logln!("=== PAGE TREE OBJECT {} ===", self.id());
        self.range_tree.lock().print_tree();
    }
}

impl Drop for Object {
    fn drop(&mut self) {
        //logln!("Dropping object {}", self.id);
    }
}

#[derive(Clone, Copy, Debug)]
pub enum InvalidateMode {
    Full,
    WriteProtect,
}

impl Default for Object {
    fn default() -> Self {
        Self::new_kernel()
    }
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
            return LookupResult::WasDeleted;
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

pub fn scan_deleted() {
    let dobjs = {
        let mut om = obj_manager().map.lock();
        om.extract_if(|_, obj| {
            if obj.is_pending_delete() {
                let ctx = obj.contexts.lock();
                let pin = obj.pin_info.lock();

                ctx.contexts.len() == 0 && pin.pins.len() == 0
            } else {
                false
            }
        })
        .collect::<Vec<_>>()
    };
    for dobj in dobjs {
        ties::TIE_MGR.delete_object(dobj.1);
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
    ties::TIE_MGR.create_object_ties(obj.id(), obj.ties.iter().map(|tie| tie.id));
    obj_manager().register_object(obj);
}

pub fn no_exist(id: ObjID) {
    obj_manager().no_exist.lock().insert(id);
}
