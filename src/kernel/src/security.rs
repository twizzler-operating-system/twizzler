use alloc::{collections::BTreeMap, sync::Arc};

use lazy_static::lazy_static;
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::SctxAttachError,
};

use crate::{
    memory::context::{KernelMemoryContext, KernelObject, ObjectContextInfo, UserContext},
    mutex::Mutex,
    obj::LookupFlags,
    spinlock::Spinlock,
    thread::current_memory_context,
};

#[derive(Clone)]
struct SecCtxMgrInner {
    active: SecurityContextRef,
    inactive: BTreeMap<ObjID, SecurityContextRef>,
}

/// Management of per-thread security context info.
pub struct SecCtxMgr {
    inner: Mutex<SecCtxMgrInner>,
    // Cache this here so we can access it quickly and without grabbing a mutex.
    active_id: Spinlock<ObjID>,
}

/// A single security context.
pub struct SecurityContext {
    kobj: Option<KernelObject<()>>,
    cache: BTreeMap<ObjID, PermsInfo>,
}

impl core::fmt::Debug for SecurityContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let id = self.kobj.as_ref().map(|ko| ko.id());
        f.debug_struct("SecurityContext")
            .field("id", &id)
            .finish_non_exhaustive()
    }
}

pub type SecurityContextRef = Arc<SecurityContext>;

/// The kernel gets a special, reserved sctx ID.
pub const KERNEL_SCTX: ObjID = ObjID::new(0);

/// Information about protections for a given object within a context.
#[derive(Clone, Copy)]
pub struct PermsInfo {
    ctx: ObjID,
    prot: Protections,
}

/// Information about how we want to access an object for perms checking.
#[derive(Clone, Copy)]
pub struct AccessInfo {
    /// The target object we're accessing
    pub target_id: ObjID,
    /// The way we are accessing the object
    pub access_kind: Protections,
    /// The object we are executing in
    pub exec_id: Option<ObjID>,
    /// Offset into the exec object for the instruction pointer
    pub exec_off: usize,
}

impl SecurityContext {
    /// Lookup the permission info for an object, and maybe cache it.
    pub fn lookup(&self, _id: ObjID) -> &PermsInfo {
        todo!()
    }

    pub fn new(kobj: Option<KernelObject<()>>) -> Self {
        Self {
            kobj,
            cache: Default::default(),
        }
    }

    pub fn id(&self) -> ObjID {
        self.kobj
            .as_ref()
            .map(|kobj| kobj.id())
            .unwrap_or(KERNEL_SCTX)
    }
}

impl SecCtxMgr {
    /// Lookup the permission info for an object in the active context, and maybe cache it.
    pub fn lookup(&self, id: ObjID) -> PermsInfo {
        *self.inner.lock().active.lookup(id)
    }

    /// Get the active context.
    pub fn active(&self) -> SecurityContextRef {
        self.inner.lock().active.clone()
    }

    /// Get the active ID. This is faster than active().id() and doesn't allocate memory (and only
    /// uses a spinlock).
    pub fn active_id(&self) -> ObjID {
        *self.active_id.lock()
    }

    /// Check access rights in the active context.
    pub fn check_active_access(&self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

    /// Search all attached contexts for access.
    pub fn search_access(&self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

    /// Build a new SctxMgr for user threads.
    pub fn new(ctx: SecurityContextRef) -> Self {
        let id = ctx.id();
        Self {
            inner: Mutex::new(SecCtxMgrInner {
                active: ctx,
                inactive: Default::default(),
            }),
            active_id: Spinlock::new(id),
        }
    }

    /// Build a new SctxMgr for kernel threads.
    pub fn new_kernel() -> Self {
        Self {
            inner: Mutex::new(SecCtxMgrInner {
                active: Arc::new(SecurityContext::new(None)),
                inactive: Default::default(),
            }),
            active_id: Spinlock::new(KERNEL_SCTX),
        }
    }

    /// Switch to the specified context.
    pub fn switch_context(&self, id: ObjID) -> SwitchResult {
        if *self.active_id.lock() == id {
            return SwitchResult::NoSwitch;
        }

        let mut inner = self.inner.lock();
        if let Some(mut ctx) = inner.inactive.remove(&id) {
            core::mem::swap(&mut ctx, &mut inner.active);
            *self.active_id.lock() = id;
            // ctx now holds the old active context
            inner.inactive.insert(ctx.id(), ctx);
            current_memory_context().map(|mc| mc.switch_to(id));
            SwitchResult::Switched
        } else {
            SwitchResult::NotAttached
        }
    }

    /// Attach a security context.
    pub fn attach(&self, sctx: SecurityContextRef) -> Result<(), SctxAttachError> {
        let mut inner = self.inner.lock();
        if inner.active.id() == sctx.id() || inner.inactive.contains_key(&sctx.id()) {
            return Err(SctxAttachError::AlreadyAttached);
        }
        inner.inactive.insert(sctx.id(), sctx);
        Ok(())
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug)]
/// Possible results of switching.
pub enum SwitchResult {
    /// No switch was needed.
    NoSwitch,
    /// Switch successful.
    Switched,
    /// The specified ID was not attached.
    NotAttached,
}

impl Clone for SecCtxMgr {
    fn clone(&self) -> Self {
        let inner = self.inner.lock().clone();
        let active_id = inner.active.id();
        Self {
            inner: Mutex::new(inner),
            active_id: Spinlock::new(active_id),
        }
    }
}

struct GlobalSecCtxMgr {
    contexts: Mutex<BTreeMap<ObjID, SecurityContextRef>>,
}

lazy_static! {
    static ref GLOBAL_SECCTX_MGR: GlobalSecCtxMgr = GlobalSecCtxMgr {
        contexts: Default::default()
    };
}

/// Get a security contexts from the global cache.
pub fn get_sctx(id: ObjID) -> Result<SecurityContextRef, SctxAttachError> {
    let obj = crate::obj::lookup_object(id, LookupFlags::empty())
        .ok_or(SctxAttachError::ObjectNotFound)?;
    let mut global = GLOBAL_SECCTX_MGR.contexts.lock();
    let entry = global.entry(id).or_insert_with(|| {
        // TODO: use control object cacher.
        let kobj =
            crate::memory::context::kernel_context().insert_kernel_object(ObjectContextInfo::new(
                obj,
                Protections::READ,
                twizzler_abi::device::CacheType::WriteBack,
            ));
        Arc::new(SecurityContext::new(Some(kobj)))
    });
    Ok(entry.clone())
}

impl Drop for SecCtxMgr {
    fn drop(&mut self) {
        let mut global = GLOBAL_SECCTX_MGR.contexts.lock();
        let inner = self.inner.lock();
        // Check the contexts we have a reference to. If the value is 2, then it's only us and the
        // global mgr that have a ref. Since we hold the global mgr lock, this will not get
        // incremented if no one else holds a ref.
        for ctx in inner.inactive.values() {
            if ctx.id() != KERNEL_SCTX && Arc::strong_count(ctx) == 2 {
                global.remove(&ctx.id());
            }
        }
        if inner.active.id() != KERNEL_SCTX && Arc::strong_count(&inner.active) == 2 {
            global.remove(&inner.active.id());
        }
    }
}

// use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

// use lazy_static::lazy_static;
// use twizzler_abi::{
//     object::{ObjID, Protections, NULLPAGE_SIZE},
//     syscall::SctxAttachError,
// };
// use twizzler_security::{sec_ctx::map::SecCtxMap, Cap, Del, Revoc};

// use crate::{
//     memory::context::{
//         KernelMemoryContext, KernelObject, KernelObjectHandle, ObjectContextInfo, UserContext,
//     },
//     mutex::Mutex,
//     obj::LookupFlags,
//     spinlock::Spinlock,
//     thread::current_memory_context,
// };

// #[derive(Clone)]
// struct SecCtxMgrInner {
//     active: SecurityContextRef,
//     // id_of_sectx -> sectx
//     inactive: BTreeMap<ObjID, SecurityContextRef>,
// }

// /// Management of per-thread security context info.
// pub struct SecCtxMgr {
//     inner: Mutex<SecCtxMgrInner>,
//     // Cache this here so we can access it quickly and without grabbing a mutex.
//     active_id: Spinlock<ObjID>,
// }

// /// A single security context.
// pub struct SecurityContext {
//     // need to read in here, this is a reference to the object from the kernel's pov that holds
// the     // security context data
//     kobj: Option<KernelObject<SecCtxMap>>,
//     // target_object_id -> permission info for that object
//     cache: Mutex<BTreeMap<ObjID, PermsInfo>>,
// }

// //TODO: would be nice if a security context shows what it has access too (printing out the
// //      caps and dels)
// impl core::fmt::Debug for SecurityContext {
//     fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
//         let id = self.kobj.as_ref().map(|ko| ko.id());
//         f.debug_struct("SecurityContext")
//             .field("id", &id)
//             .finish_non_exhaustive()
//     }
// }

// pub type SecurityContextRef = Arc<SecurityContext>;

// /// The kernel gets a special, reserved sctx ID.
// pub const KERNEL_SCTX: ObjID = ObjID::new(0);

// /// Information about protections for a given object within a context.
// #[derive(Clone, Copy)]
// pub struct PermsInfo {
//     /// the id of the context that provides the protections
//     ctx: ObjID,
//     prot: Protections,
//     //TODO: think about how this would work later
//     // revoc: Revoc
// }

// impl PermsInfo {
//     pub fn new(ctx: ObjID, prot: Protections) -> Self {
//         Self { ctx, prot }
//     }
// }

// /// Information about how we want to access an object for perms checking.
// #[derive(Clone, Copy)]
// pub struct AccessInfo {
//     /// The target object we're accessing
//     pub target_id: ObjID,
//     /// The way we are accessing the object
//     pub access_kind: Protections,
//     /// The object we are executing in
//     pub exec_id: Option<ObjID>,
//     /// Offset into the exec object for the instruction pointer
//     pub exec_off: usize,
// }

// impl SecurityContext {
//     /// Lookup the permission info for an object, and maybe cache it.
//     pub fn lookup(&self, _id: ObjID) -> PermsInfo {
//         if let Some(perms_info) = self.cache.get(&_id) {
//             return *perms_info;
//         }

//         // secctxmap is always going to be at the base of the kobj
//         let map = self.kobj.unwrap().base::<SecCtxMap>();
//         // .lea_raw::<SecCtxMap>(NULLPAGE_SIZE)
//         // .unwrap();

//         // // example of how to use lea_raw
//         let x = self
//             .kobj
//             .unwrap()
//             .lea_raw::<CapOrDel>(NULLPAGE_SIZE + 1024 * n);

//         // cache miss
//         let mut perms = PermsInfo::new(self.id(), Protections::empty());
//         for cap in self.caps.iter() {
//             //NOTE: like do we verify here?
//             // cap.verify_sig(verifying_key);
//             // need to store / get the verification keys from somewhere
//             if cap.target == _id {
//                 perms.prot |= cap.protections; // union of all protections granted
//             }
//         }

//         self.cache.insert(_id, perms);
//         // unsure how else id get a reference out
//         *self.cache.get(&_id).unwrap()
//     }

//     pub fn new(kobj: Option<KernelObject<()>>) -> Self {
//         Self {
//             kobj,
//             cache: Default::default(),
//         }
//     }

//     pub fn id(&self) -> ObjID {
//         self.kobj
//             .as_ref()
//             .map(|kobj| kobj.id())
//             .unwrap_or(KERNEL_SCTX)
//     }
// }

// impl SecCtxMgr {
//     /// Lookup the permission info for an object in the active context, and maybe cache it.
//     pub fn lookup(&mut self, id: ObjID) -> PermsInfo {
//         self.inner.lock().active.lookup(id)
//     }

//     /// Get the active context.
//     pub fn active(&self) -> SecurityContextRef {
//         self.inner.lock().active.clone()
//     }

//     /// Get the active ID. This is faster than active().id() and doesn't allocate memory (and
// only     /// uses a spinlock).
//     pub fn active_id(&self) -> ObjID {
//         *self.active_id.lock()
//     }

//     /// Check access rights in the active context.
//     pub fn check_active_access(&mut self, _access_info: AccessInfo) -> &PermsInfo {
//         // has to check 2 things
//         // 1) if the memory access is allowed in this context
//         // 2) If the instruction that did the access may be executed in this context (no idea how
// to         //    implement this lmfao)
//         let perms = self.inner.lock().active.lookup(_access_info.target_id);

//         todo!("figure out how to get the instruction / checking it here")
//     }

//     /// Search all attached contexts for access.
//     pub fn search_access(&self, _access_info: AccessInfo) -> &PermsInfo {
//         todo!("just do it")
//     }

//     /// Build a new SctxMgr for user threads.
//     pub fn new(ctx: SecurityContextRef) -> Self {
//         let id = ctx.id();
//         Self {
//             inner: Mutex::new(SecCtxMgrInner {
//                 active: ctx,
//                 inactive: Default::default(),
//             }),
//             active_id: Spinlock::new(id),
//         }
//     }

//     /// Build a new SctxMgr for kernel threads.
//     pub fn new_kernel() -> Self {
//         Self {
//             inner: Mutex::new(SecCtxMgrInner {
//                 active: Arc::new(SecurityContext::new(None)),
//                 inactive: Default::default(),
//             }),
//             active_id: Spinlock::new(KERNEL_SCTX),
//         }
//     }

//     /// Switch to the specified context.
//     pub fn switch_context(&self, id: ObjID) -> SwitchResult {
//         if *self.active_id.lock() == id {
//             return SwitchResult::NoSwitch;
//         }

//         let mut inner = self.inner.lock();
//         if let Some(mut ctx) = inner.inactive.remove(&id) {
//             core::mem::swap(&mut ctx, &mut inner.active);
//             *self.active_id.lock() = id;
//             // ctx now holds the old active context
//             inner.inactive.insert(ctx.id(), ctx);
//             current_memory_context().map(|mc| mc.switch_to(id));
//             SwitchResult::Switched
//         } else {
//             SwitchResult::NotAttached
//         }
//     }

//     /// Attach a security context.
//     pub fn attach(&self, sctx: SecurityContextRef) -> Result<(), SctxAttachError> {
//         let mut inner = self.inner.lock();
//         if inner.active.id() == sctx.id() || inner.inactive.contains_key(&sctx.id()) {
//             return Err(SctxAttachError::AlreadyAttached);
//         }
//         inner.inactive.insert(sctx.id(), sctx);
//         Ok(())
//     }
// }

// #[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Debug)]
// /// Possible results of switching.
// pub enum SwitchResult {
//     /// No switch was needed.
//     NoSwitch,
//     /// Switch successful.
//     Switched,
//     /// The specified ID was not attached.
//     NotAttached,
// }

// impl Clone for SecCtxMgr {
//     fn clone(&self) -> Self {
//         let inner = self.inner.lock().clone();
//         let active_id = inner.active.id();
//         Self {
//             inner: Mutex::new(inner),
//             active_id: Spinlock::new(active_id),
//         }
//     }
// }

// struct GlobalSecCtxMgr {
//     contexts: Mutex<BTreeMap<ObjID, SecurityContextRef>>,
// }

// lazy_static! {
//     static ref GLOBAL_SECCTX_MGR: GlobalSecCtxMgr = GlobalSecCtxMgr {
//         contexts: Default::default()
//     };
// }

// /// Get a security contexts from the global cache.
// pub fn get_sctx(id: ObjID) -> Result<SecurityContextRef, SctxAttachError> {
//     let obj = crate::obj::lookup_object(id, LookupFlags::empty())
//         .ok_or(SctxAttachError::ObjectNotFound)?;
//     let mut global = GLOBAL_SECCTX_MGR.contexts.lock();
//     let entry = global.entry(id).or_insert_with(|| {
//         // TODO: use control object cacher.
//         let kobj =
//             crate::memory::context::kernel_context().insert_kernel_object(ObjectContextInfo::new(
//                 obj,
//                 Protections::READ,
//                 twizzler_abi::device::CacheType::WriteBack,
//             ));
//         Arc::new(SecurityContext::new(Some(kobj)))
//     });
//     Ok(entry.clone())
// }

// impl Drop for SecCtxMgr {
//     fn drop(&mut self) {
//         let mut global = GLOBAL_SECCTX_MGR.contexts.lock();
//         let inner = self.inner.lock();
//         // Check the contexts we have a reference to. If the value is 2, then it's only us and
// the         // global mgr that have a ref. Since we hold the global mgr lock, this will not get
//         // incremented if no one else holds a ref.
//         for ctx in inner.inactive.values() {
//             if ctx.id() != KERNEL_SCTX && Arc::strong_count(ctx) == 2 {
//                 global.remove(&ctx.id());
//             }
//         }
//         if inner.active.id() != KERNEL_SCTX && Arc::strong_count(&inner.active) == 2 {
//             global.remove(&inner.active.id());
//         }
//     }
// }
