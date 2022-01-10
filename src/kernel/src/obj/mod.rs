use core::sync::atomic::{AtomicU32, Ordering};

use alloc::{collections::BTreeMap, sync::Arc};

use crate::mutex::{LockGuard, Mutex};

use self::pages::PageRef;

pub mod pages;
pub mod pagevec;
pub mod range;

pub type ObjID = u128; //TODO: pull this in from twz-abi?

const OBJ_DELETED: u32 = 1;
pub struct Object {
    id: ObjID,
    flags: AtomicU32,
    range_tree: Mutex<range::RangeTree>,
}

#[derive(Clone, Copy, Debug, PartialOrd, Ord, PartialEq, Eq)]
#[repr(transparent)]
pub struct PageNumber(usize);

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
}

impl Object {
    pub fn is_pending_delete(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & OBJ_DELETED != 0
    }

    pub fn mark_for_delete(&self) {
        self.flags.fetch_or(OBJ_DELETED, Ordering::SeqCst);
    }

    pub fn lock_page_tree(&self) -> LockGuard<'_, range::RangeTree> {
        self.range_tree.lock()
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
}

lazy_static::lazy_static! {
    static ref OBJ_MANAGER: ObjectManager = ObjectManager::new();
}

pub fn lookup_object(id: ObjID, flags: LookupFlags) -> LookupResult {
    let om = &OBJ_MANAGER;
    om.lookup_object(id, flags)
}
