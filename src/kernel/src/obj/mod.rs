use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, Ordering},
};

use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use twizzler_abi::object::ObjID;

use crate::{
    idcounter::{IdCounter, SimpleId},
    memory::{context::MappingRef, PhysAddr, VirtAddr},
    mutex::{LockGuard, Mutex},
};

use self::{pages::Page, thread_sync::SleepInfo};

pub mod copy;
pub mod pages;
pub mod pagevec;
pub mod range;
pub mod thread_sync;

const OBJ_DELETED: u32 = 1;
pub struct Object {
    id: ObjID,
    flags: AtomicU32,
    range_tree: Mutex<range::PageRangeTree>,
    maplist: Mutex<BTreeSet<MappingRef>>,
    sleep_info: Mutex<SleepInfo>,
    pin_info: Mutex<PinInfo>,
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

    pub const PAGE_SIZE: usize = 0x1000; //TODO: arch-dep

    pub fn from_address(addr: VirtAddr) -> Self {
        PageNumber(((addr.raw() % (1 << 30)) / 0x1000) as usize) //TODO: arch-dep
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

static OID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
impl Object {
    pub fn is_pending_delete(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & OBJ_DELETED != 0
    }

    pub fn mark_for_delete(&self) {
        self.flags.fetch_or(OBJ_DELETED, Ordering::SeqCst);
    }

    pub fn lock_page_tree(&self) -> LockGuard<'_, range::PageRangeTree> {
        self.range_tree.lock()
    }

    pub fn add_page(&self, pn: PageNumber, page: pages::Page) {
        let mut range_tree = self.range_tree.lock();
        range_tree.add_page(pn, page);
    }

    pub fn id(&self) -> ObjID {
        self.id
    }

    pub fn insert_mapping(&self, mapping: MappingRef) {
        self.maplist.lock().insert(mapping);
    }

    pub fn release_pin(&self, _pin: u32) {
        // TODO: Currently we don't track pins. This will be changed in-future when we fully implement eviction.
    }

    pub fn pin(&self, start: PageNumber, len: usize) -> Option<(Vec<PhysAddr>, u32)> {
        let mut tree = self.lock_page_tree();

        let mut pin_info = self.pin_info.lock();

        let mut v = Vec::new();
        for i in 0..len {
            // TODO: we'll need to handle failures here when we expand the paging system.
            let p = tree.get_page(start.offset(i), true);
            if let Some(p) = p {
                v.push(p.0.physical_address());
            } else {
                let page = Page::new();
                v.push(page.physical_address());
                tree.add_page(start.offset(i), page);
            }
        }

        let id = pin_info.id_counter.next_simple();
        let token = id.value().try_into().ok()?;
        pin_info.pins.push(id);

        Some((v, token))
    }

    pub fn new() -> Self {
        Self {
            id: ((OID.fetch_add(1, Ordering::SeqCst) as u128) | (1u128 << 64)).into(),
            flags: AtomicU32::new(0),
            range_tree: Mutex::new(range::PageRangeTree::new()),
            maplist: Mutex::new(BTreeSet::new()),
            sleep_info: Mutex::new(SleepInfo::new()),
            pin_info: Mutex::new(PinInfo::default()),
        }
    }

    pub fn invalidate(&self, _range: core::ops::Range<PageNumber>) {
        //todo!()
    }

    pub fn print_page_tree(&self) {
        logln!("=== PAGE TREE OBJECT {} ===", self.id());
        self.range_tree.lock().print_tree();
    }
}

impl Default for Object {
    fn default() -> Self {
        Self::new()
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
}

bitflags::bitflags! {
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
        }
    }

    fn lookup_object(&self, id: ObjID, flags: LookupFlags) -> LookupResult {
        self.map
            .lock()
            .get(&id)
            .map_or(LookupResult::NotFound, |obj| {
                if !obj.is_pending_delete() || flags.contains(LookupFlags::ALLOW_DELETED) {
                    LookupResult::Found(obj.clone())
                } else {
                    LookupResult::WasDeleted
                }
            })
    }

    fn register_object(&self, obj: Arc<Object>) {
        // TODO: what if it returns an obj
        self.map.lock().insert(obj.id(), obj);
    }
}

lazy_static::lazy_static! {
    static ref OBJ_MANAGER: ObjectManager = ObjectManager::new();
}

pub fn lookup_object(id: ObjID, flags: LookupFlags) -> LookupResult {
    let om = &OBJ_MANAGER;
    om.lookup_object(id, flags)
}

pub fn register_object(obj: Arc<Object>) {
    let om = &OBJ_MANAGER;
    om.register_object(obj);
}
