use core::sync::atomic::{AtomicU32, Ordering};

use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{
    memory::context::MappingRef,
    mutex::{LockGuard, Mutex},
};

pub mod copy;
pub mod pages;
pub mod pagevec;
pub mod range;

pub type ObjID = u128; //TODO: pull this in from twz-abi?

const OBJ_DELETED: u32 = 1;
pub struct Object {
    id: ObjID,
    flags: AtomicU32,
    range_tree: Mutex<range::PageRangeTree>,
    maplist: Mutex<BTreeSet<MappingRef>>,
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

    pub fn from_address(addr: x86_64::VirtAddr) -> Self {
        PageNumber(((addr.as_u64() % (1 << 30)) / 0x1000) as usize) //TODO: arch-dep
    }

    pub fn next(&self) -> Self {
        Self(self.0 + 1)
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

    pub fn new() -> Self {
        Self {
            id: OID.fetch_add(1, Ordering::SeqCst) as u128,
            flags: AtomicU32::new(0),
            range_tree: Mutex::new(range::PageRangeTree::new()),
            maplist: Mutex::new(BTreeSet::new()),
        }
    }

    pub fn invalidate(&self, _range: core::ops::Range<PageNumber>) {
        todo!()
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

pub type ObjectRef = Arc<Object>;

struct ObjectManager {
    map: Mutex<BTreeMap<ObjID, ObjectRef>>,
}

bitflags::bitflags! {
    pub struct LookupFlags: u32 {
        const ALLOW_DELETED = 1;
    }
}

pub enum LookupResult {
    NotFound,
    WasDeleted,
    Pending,
    Found(ObjectRef),
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

    fn register_object(&self, obj: Object) {
        // TODO: what if it returns an obj
        self.map.lock().insert(obj.id(), Arc::new(obj));
    }
}

lazy_static::lazy_static! {
    static ref OBJ_MANAGER: ObjectManager = ObjectManager::new();
}

pub fn lookup_object(id: ObjID, flags: LookupFlags) -> LookupResult {
    let om = &OBJ_MANAGER;
    om.lookup_object(id, flags)
}

pub fn register_object(obj: Object) {
    let om = &OBJ_MANAGER;
    om.register_object(obj);
}
