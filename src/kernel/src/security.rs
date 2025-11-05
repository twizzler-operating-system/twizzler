use alloc::{collections::BTreeMap, sync::Arc};

use log::info;
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    syscall::MapFlags,
};
use twizzler_rt_abi::error::{NamingError, ObjectError};
pub use twizzler_security::PermsInfo;
use twizzler_security::{Cap, CtxMapItemType, SecCtxBase, VerifyingKey};

use crate::{
    memory::context::{
        kernel_context, KernelMemoryContext, KernelObject, KernelObjectHandle, ObjectContextInfo,
        UserContext,
    },
    mutex::Mutex,
    obj::{lookup_object, LookupFlags, LookupResult},
    once::Once,
    spinlock::Spinlock,
    thread::current_memory_context,
};

#[derive(Clone)]
struct SecCtxMgrInner {
    active: SecurityContextRef,
    //ObjID here refers to the security contexts ID
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
    kobj: Option<KernelObject<SecCtxBase>>,
    cache: Mutex<BTreeMap<ObjID, PermsInfo>>,
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
    pub fn lookup(&self, _id: ObjID) -> PermsInfo {
        // check the cache to see if we already have something
        if let Some(cache_entry) = self.cache.lock().get(&_id) {
            return *cache_entry;
        }

        // by default granted permissions are going to be the most restrictive
        let mut granted_perms =
            // PermsInfo::new(self.id(), Protections::all(), Protections::empty());
        PermsInfo::new(self.id(), Protections::empty(), Protections::empty());
        //

        info!("performing kobj detection check for object: {_id:#?}");
        let Some(ref obj) = self.kobj else {
            info!("there is no object backing this security context, giving default permissions!");
            // if there is no object underneath the kobj, return nothing;
            return granted_perms;
        };

        let kobj_id = obj.id();

        info!("accessing base for obj: {kobj_id:#?}");
        let base = obj.base();
        info!("succesfully accessed base for object: {kobj_id:#?}");

        info!("accessing base.map for object: {kobj_id:#?}");
        // check for possible items
        let Some(results) = base.map.get(&_id) else {
            info!("there are no capabilites or delegations for target object: {_id:#?}");
            // if no entries for the target, return already granted perms
            return granted_perms;
        };
        info!("finished acessing base.map for object: {_id:#?}");
        let v_obj = {
            // so far, we are never able to reach this point
            info!("looking up object: {_id:#?}");
            let target_obj = match lookup_object(_id, LookupFlags::empty()) {
                LookupResult::Found(obj) => obj,
                _ => return granted_perms,
            };

            info!("found object: {target_obj:#?}");
            let Some(meta) = target_obj.read_meta(true) else {
                // failed to read meta, no perms granted
                return granted_perms;
            };
            info!("found object metadata: {meta:#?}");
            match lookup_object(meta.kuid, LookupFlags::empty()) {
                LookupResult::Found(v_obj) => {
                    info!("found verifying key! {v_obj:#?}");
                    let k_ctx = kernel_context();

                    let handle =
                        k_ctx.insert_kernel_object::<VerifyingKey>(ObjectContextInfo::new(
                            v_obj,
                            Protections::READ,
                            CacheType::WriteBack,
                            MapFlags::STABLE,
                        ));
                    handle
                }
                // verifying key wasnt found, return no perms
                _ => return granted_perms,
            }
        };

        let v_key = v_obj.base();

        for entry in results {
            match entry.item_type {
                CtxMapItemType::Del => {
                    todo!("Delegations not supported yet for lookup")
                }

                CtxMapItemType::Cap => {
                    //NOTE: is this going to return the same as Object.lea?
                    let Some(cap) = obj.lea_raw(entry.offset as *const Cap) else {
                        // something weird going on, entry offset not inside object bounds,
                        // return already granted perms to avoid panic
                        return granted_perms;
                    };

                    if cap.verify_sig(v_key).is_ok() {
                        granted_perms.provide = granted_perms.provide | cap.protections;
                    };
                }
            }
        }

        // lookup mask for obj in base
        let Some(mask) = base.masks.get(&_id) else {
            // no mask for target object
            // final perms are granted_perms & global_mask
            granted_perms.provide &= base.global_mask;
            self.cache.lock().insert(_id, granted_perms.clone());
            return granted_perms;
        };

        // final permissions will be ,
        // granted_perms & permmask & (global_mask | override_mask)
        granted_perms.provide =
            granted_perms.provide & mask.permmask & (base.global_mask | mask.ovrmask);

        // insert into cache
        self.cache.lock().insert(_id, granted_perms.clone());

        granted_perms
    }

    pub fn new(kobj: Option<KernelObject<SecCtxBase>>) -> Self {
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
        let active = self.active();
        active.lookup(id)
        // in this function we would have to call lookup without calling the lock on self.inner()
        // self.inner.lock().active.lookup(id)
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
    pub fn check_active_access(&self, _access_info: &AccessInfo) -> PermsInfo {
        //TODO: will probably have to hook up the gate check here as well?
        // WARN: actually doing the lookup is causing the kernel to die so just skipping that for
        // now for some reason
        let perms = self.lookup(_access_info.target_id);
        // let perms = PermsInfo {
        //     ctx: self.active_id(),
        //     provide: Protections::all(),
        //     restrict: Protections::empty(),
        // };

        perms
    }

    /// Search all attached contexts for access.
    pub fn search_access(&self, _access_info: &AccessInfo) -> PermsInfo {
        //TODO: need to actually look through all the contexts, this is just temporary
        // let mut greatest_perms = self.lookup(_access_info.target_id);

        // for (_, ctx) in &self.inner.lock().inactive {
        //     let perms = ctx.lookup(_access_info.target_id);
        //     // how do you determine what prots is more expressive? like more
        //     // lets just return if its anything other than empty
        //     if perms.provide & !perms.restrict != Protections::empty() {
        //         greatest_perms = perms
        //     }
        // }
        // greatest_perms

        PermsInfo {
            ctx: self.active_id(),
            provide: Protections::all(),
            restrict: Protections::empty(),
        }
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

        info!("trying to acquired guard");
        let mut inner = self.inner.lock();
        info!("called to switch to id: {id:#?}");

        let ret = if let Some(mut ctx) = inner.inactive.remove(&id) {
            core::mem::swap(&mut ctx, &mut inner.active);

            *self.active_id.lock() = id;
            // ctx now holds the old active context
            inner.inactive.insert(ctx.id(), ctx);
            current_memory_context().map(|mc| mc.switch_to(id));
            SwitchResult::Switched
        } else {
            SwitchResult::NotAttached
        };

        info!("dropped guard!");
        ret
    }

    /// Attach a security context.
    pub fn attach(&self, sctx: SecurityContextRef) -> twizzler_rt_abi::Result<()> {
        let mut inner = self.inner.lock();
        if inner.active.id() == sctx.id() || inner.inactive.contains_key(&sctx.id()) {
            return Err(NamingError::AlreadyBound.into());
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

static GLOBAL_SECCTX_MGR: Once<GlobalSecCtxMgr> = Once::new();

fn global_secctx_mgr() -> &'static GlobalSecCtxMgr {
    GLOBAL_SECCTX_MGR.call_once(|| GlobalSecCtxMgr {
        contexts: Default::default(),
    })
}

/// Get a security contexts from the global cache.
pub fn get_sctx(id: ObjID) -> twizzler_rt_abi::Result<SecurityContextRef> {
    let obj =
        crate::obj::lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
    let mut global = global_secctx_mgr().contexts.lock();
    let entry = global.entry(id).or_insert_with(|| {
        // TODO: use control object cacher.
        let kobj =
            crate::memory::context::kernel_context().insert_kernel_object(ObjectContextInfo::new(
                obj,
                Protections::READ,
                twizzler_abi::device::CacheType::WriteBack,
                MapFlags::empty(),
            ));
        Arc::new(SecurityContext::new(Some(kobj)))
    });
    Ok(entry.clone())
}

impl Drop for SecCtxMgr {
    fn drop(&mut self) {
        let mut global = global_secctx_mgr().contexts.lock();
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

mod tests {
    use core::hint::black_box;

    use twizzler_abi::object::Protections;
    use twizzler_kernel_macros::kernel_test;
    use twizzler_security::{Cap, SigningKey, SigningScheme};

    use crate::{random::getrandom, utils::benchmark};
    #[kernel_test]
    fn bench_capability_verification() {
        let mut rand_bytes = [0; 32];

        getrandom(&mut rand_bytes, false);

        let (s_key, v_key) = SigningKey::new_kernel_keypair(&SigningScheme::Ecdsa, rand_bytes)
            .expect("shouldnt have errored");

        let cap = Cap::new(
            0x123.into(),
            0x100.into(),
            Protections::all(),
            &s_key,
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .expect("capability creation shouldnt have errored");

        benchmark(|| {
            let _x = black_box(cap.verify_sig(&v_key).expect("should succeed"));
        });
    }

    //TODO: write a thorough security context test when that stuff is implemented
}
