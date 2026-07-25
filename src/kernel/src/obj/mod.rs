use alloc::{
    boxed::Box,
    collections::{BTreeMap, btree_set::BTreeSet},
    sync::Arc,
    vec::Vec,
};
use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use twizzler_abi::{
    device::NUM_DEVICE_INTERRUPTS,
    meta::{MetaFlags, MetaInfo},
    object::{MAX_SIZE, ObjID, Protections},
    syscall::{BackingType, LifetimeType, ObjectInfo},
};
use twizzler_rt_abi::{bindings::object_tie, error::TwzError, object::Nonce};

use self::thread_sync::SleepInfo;
use crate::{
    arch::memory::frame::FRAME_SIZE,
    idcounter::{IdCounter, SimpleId},
    memory::VirtAddr,
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
pub struct Object {
    pub id: ObjID,
    flags: AtomicU32,
    tables: Mutex<pagetables::ObjectPageTable>,
    sleep_info: Mutex<SleepInfo>,
    device_interrupt_info: Box<[(AtomicU64, AtomicU64); NUM_DEVICE_INTERRUPTS]>,
    pin_info: Mutex<PinInfo>,
    lifetime_type: LifetimeType,
    ties: Vec<object_tie>,
    verified_id: OnceWait<(bool, Protections)>,
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

    pub fn is_kernel_id(&self) -> bool {
        self.id.parts()[0] == 1
    }

    pub fn mark_for_delete(&self) {
        self.flags.fetch_or(OBJ_DELETED, Ordering::SeqCst);
    }

    #[track_caller]
    pub fn lock_page_tables(&self) -> LockGuard<'_, pagetables::ObjectPageTable> {
        self.tables.lock()
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
            pin_info: Mutex::new(PinInfo::default()),
            ties: ties.to_vec(),
            verified_id: OnceWait::new(),
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
        while !obj.write_meta(meta) {
            logln!("failed to write object metadata -- retrying");
        }
        obj
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
            while !obj.write_meta(meta) {
                panic!("failed to write object metadata");
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
        let obj = Arc::new(obj);
        while !obj.write_meta(meta) {
            logln!("failed to write object metadata -- retrying");
        }
        obj
    }

    pub fn print_page_tree(&self) {
        logln!("=== PAGE TREE OBJECT {} ===", self.id());
        self.tables.lock().print_tree();
    }

    pub fn info(&self) -> ObjectInfo {
        let (num_pages, maps) = {
            let page_tree = self.tables.lock();
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
    let dobjs = {
        let mut om = obj_manager().map.lock();
        om.extract_if(.., |_, obj| {
            if obj.is_pending_delete() {
                let not_mapped = obj.lock_page_tables().map_count() == 0;
                let pin = obj.pin_info.lock();

                not_mapped && pin.pins.len() == 0
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
    ties::TIE_MGR.create_object_ties(obj.id(), obj.ties.iter().map(|tie| tie.id.into()));
    obj_manager().register_object(obj);
}

pub fn no_exist(id: ObjID) {
    obj_manager().no_exist.lock().insert(id);
}

pub fn is_no_exist(id: ObjID) -> bool {
    obj_manager().no_exist.lock().contains(&id)
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

pub fn enumerate_objects(_buf: &mut [ObjID], _offset: usize) -> Result<usize, TwzError> {
    Err(TwzError::NOT_SUPPORTED)
}
