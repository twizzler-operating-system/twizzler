use alloc::{
    collections::{btree_map::Entry, btree_set::BTreeSet, BTreeMap},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, Ordering},
};

use range::PageStatus;
use twizzler_abi::{
    meta::MetaFlags,
    object::{ObjID, MAX_SIZE},
    syscall::{CreateTieSpec, LifetimeType},
};

use self::{pages::Page, thread_sync::SleepInfo};
use crate::{
    arch::memory::frame::FRAME_SIZE,
    idcounter::{IdCounter, SimpleId, StableId},
    memory::{
        context::{kernel_context, Context, ContextRef, UserContext},
        PhysAddr, VirtAddr,
    },
    mutex::{LockGuard, Mutex},
};

pub mod control;
pub mod copy;
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

static OID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

fn backup_id_gen() -> ObjID {
    ((OID.fetch_add(1, Ordering::SeqCst) as u128) | (1u128 << 64)).into()
}

fn gen_id(nonce: ObjID, kuid: ObjID, flags: MetaFlags) -> ObjID {
    #[repr(C)]
    struct Ids {
        nonce: ObjID,
        kuid: ObjID,
        flags: MetaFlags,
    }
    let mut ids = Ids { nonce, kuid, flags };
    let ptr = core::ptr::addr_of_mut!(ids).cast::<u8>();
    let slice = unsafe { core::slice::from_raw_parts_mut(ptr, size_of::<Ids>()) };
    let hash = crate::crypto::sha256(slice);
    let mut id_buf = [0u8; 16];
    id_buf.copy_from_slice(&hash[0..16]);
    for i in 0..16 {
        id_buf[i] ^= hash[i + 16];
    }
    u128::from_ne_bytes(id_buf).into()
}

pub fn calculate_new_id(kuid: ObjID, flags: MetaFlags) -> ObjID {
    let mut buf = [0u8; 16];
    if !crate::random::getrandom(&mut buf, true) {
        return backup_id_gen();
    }
    let nonce = u128::from_ne_bytes(buf);
    gen_id(nonce.into(), kuid, flags)
}

impl Object {
    pub fn is_pending_delete(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & OBJ_DELETED != 0
    }

    pub fn use_pager(&self) -> bool {
        self.lifetime_type == LifetimeType::Persistent
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
            let p = tree.get_page(start.offset(i), true);
            if let PageStatus::Ready(p, _) = p {
                v.push(p.physical_address());
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

    pub fn new(id: ObjID, lifetime_type: LifetimeType, ties: &[CreateTieSpec]) -> Self {
        Self {
            id,
            flags: AtomicU32::new(0),
            range_tree: Mutex::new(range::PageRangeTree::new()),
            sleep_info: Mutex::new(SleepInfo::new()),
            pin_info: Mutex::new(PinInfo::default()),
            contexts: Mutex::new(ContextInfo::default()),
            ties: ties.to_vec(),
            lifetime_type,
        }
    }

    pub fn new_kernel() -> Self {
        Self::new(
            calculate_new_id(0.into(), MetaFlags::default()),
            LifetimeType::Volatile,
            &[],
        )
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
        logln!("Dropping object {}", self.id);
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

    fn lookup_object(&self, id: ObjID, flags: LookupFlags) -> LookupResult {
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
        logln!("checking ties for {}", id);
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
    logln!("scanning deleted");
    let dobjs = {
        let mut om = OBJ_MANAGER.map.lock();
        om.extract_if(|_, obj| {
            if obj.is_pending_delete() {
                let ctx = obj.contexts.lock();
                let pin = obj.pin_info.lock();

                logln!(
                    "checking object: {}: {} {} ",
                    obj.id,
                    ctx.contexts.len(),
                    pin.pins.len()
                );
                ctx.contexts.len() == 0 && pin.pins.len() == 0
            } else {
                false
            }
        })
        .collect::<Vec<_>>()
    };
    for dobj in dobjs {
        logln!("delete object: {}", dobj.0);
        ties::TIE_MGR.delete_object(dobj.1);
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
    ties::TIE_MGR.create_object_ties(obj.id(), obj.ties.iter().map(|tie| tie.id));
    om.register_object(obj);
}

pub fn no_exist(id: ObjID) {
    OBJ_MANAGER.no_exist.lock().insert(id);
}
