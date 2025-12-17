use alloc::{collections::BTreeMap, sync::Arc};

use heapless::index_map::FnvIndexMap;
use log::{error, trace};
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    syscall::MapFlags,
};
use twizzler_rt_abi::error::{NamingError, ObjectError};
pub use twizzler_security::PermsInfo;
use twizzler_security::{Cap, CtxMapItemType, SecCtxBase, SecCtxFlags, VerifyingKey};

use crate::{
    memory::context::{
        kernel_context, KernelMemoryContext, KernelObject, KernelObjectHandle, ObjectContextInfo,
        UserContext,
    },
    mutex::Mutex,
    obj::{lookup_object, LookupFlags, LookupResult},
    once::Once,
    spinlock::Spinlock,
    thread::{current_memory_context, current_thread_ref},
};

struct SecCtxMgrInner {
    active: SecurityContextRef,
    //ObjID here refers to the security contexts ID
    inactive: heapless::index_map::FnvIndexMap<ObjID, SecurityContextRef, 512>,
    //inactive: BTreeMap<ObjID, SecurityContextRef>,
}

impl Clone for SecCtxMgrInner {
    fn clone(&self) -> Self {
        let inactive = self
            .inactive
            .iter()
            .map(|x| (*x.0, x.1.clone()))
            .collect::<FnvIndexMap<ObjID, SecurityContextRef, 512>>();
        Self {
            active: self.active.clone(),
            inactive,
        }
    }
}

/// Management of per-thread security context info.
pub struct SecCtxMgr {
    inner: Spinlock<SecCtxMgrInner>,
    thid: u64,
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
    pub fn flags(&self) -> Option<SecCtxFlags> {
        let obj = self.kobj.as_ref()?;
        let base = obj.base();
        Some(base.flags.clone())
    }

    /// Lookup the permission info for an object, and maybe cache it.
    pub fn lookup(&self, _id: ObjID, default_prots: Protections) -> PermsInfo {
        // check the cache to see if we already have something
        if let Some(cache_entry) = self.cache.lock().get(&_id) {
            return *cache_entry;
        }

        // by default granted permissions are going to be the most restrictive
        let mut granted_perms =
            PermsInfo::new(self.id(), Protections::empty(), Protections::empty());

        // add default perms here
        granted_perms.provide = granted_perms.provide | default_prots;

        let Some(ref obj) = self.kobj else {
            // if there is no object underneath the kobj, return nothing;
            return granted_perms;
        };

        let base = obj.base();

        // check for possible items
        let Some(results) = base.map.get(&_id) else {
            // if there arent any items inside this context, just return default perms
            return granted_perms;
        };

        // from now on, whenever we return granted_perms, it must be &'d with the sec_ctx global
        // mask, since there are some entries inside the base.map()

        let v_obj = {
            let target_obj = match lookup_object(_id, LookupFlags::empty()) {
                LookupResult::Found(obj) => obj,
                _ => {
                    granted_perms.provide &= base.global_mask;
                    return granted_perms;
                }
            };

            let Some(meta) = target_obj.read_meta(true) else {
                granted_perms.provide &= base.global_mask;
                return granted_perms;
            };

            match lookup_object(meta.kuid, LookupFlags::empty()) {
                LookupResult::Found(v_obj) => {
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
                _ => {
                    granted_perms.provide &= base.global_mask;
                    return granted_perms;
                }
            }
        };

        let v_key = v_obj.base();

        for entry in results {
            match entry.item_type {
                CtxMapItemType::Del => {
                    todo!("Delegations not supported yet for lookup")
                }

                CtxMapItemType::Cap => {
                    let Some(cap) = obj.lea_raw(entry.offset as *const Cap) else {
                        error!("Failed to map capability from entry: {entry:#?}");
                        // something weird going on, entry offset not inside object bounds,
                        // return already granted perms to avoid panic
                        granted_perms.provide &= base.global_mask;
                        return granted_perms;
                    };

                    if cap.verify_sig(v_key).is_ok() {
                        granted_perms.provide |= cap.protections;
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

        // final permissions will be:
        // granted_perms & permmask & (global_mask | override_mask)
        granted_perms.provide =
            granted_perms.provide & mask.permmask & (base.global_mask | mask.ovrmask);
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
    pub fn lookup(&self, id: ObjID, default_prots: Protections) -> PermsInfo {
        self.active().lookup(id, default_prots)
    }

    #[track_caller]
    pub fn list_all(&self) {
        logln!(
            "{} :: {} {}",
            self.thid,
            self.active_id(),
            core::panic::Location::caller()
        );
        logln!("{} :: :: {:?}", self.thid, self.inner.lock().inactive);
    }

    /// Get the active context.
    pub fn active(&self) -> SecurityContextRef {
        self.inner.lock().active.clone()
    }

    /// Get the active ID. This is faster than active().id() and doesn't allocate memory (and only
    /// uses a spinlock).
    pub fn active_id(&self) -> ObjID {
        self.inner.lock().active.id()
    }

    /// Check access rights in the active context.
    pub fn check_active_access(
        &self,
        _access_info: &AccessInfo,
        default_prots: Protections,
    ) -> PermsInfo {
        let perms = self.lookup(_access_info.target_id, default_prots);
        perms
    }

    /// Search all attached contexts for access.
    pub fn search_access(&self, access_info: &AccessInfo, default_prots: Protections) -> PermsInfo {
        let active_perms = self.lookup(access_info.target_id, default_prots);

        let perms_satisfy = |granting: &PermsInfo| -> bool {
            // this is the same boolean expr used by the fault handler to check perms
            access_info.access_kind & !granting.restrict & granting.provide
                == access_info.access_kind
        };

        // the active_perms satisfy the way we are accessing the object, just return
        if perms_satisfy(&active_perms) {
            return active_perms;
        };

        // if the active context has the undetachable bit set, we cant leave it, return what we
        // already have
        if let Some(flags) = self.active().flags()
            && flags.contains(SecCtxFlags::UNDETACHABLE)
        {
            trace!("UNDETACHABLE bit set, refusing to evaluate inactive security contexts.");
            return active_perms;
        };

        // look through the other attached contexts to see if any of them match
        for (_, ctx) in &self.inner.lock().inactive {
            let perms = ctx.lookup(access_info.target_id, default_prots);

            // the perms granted by this ctx are equal to the way we are accessing the object, so
            // lets send it
            if perms_satisfy(&perms) {
                return perms;
            }
        }

        // we couldnt find an exact match to the access kind, return the (inadequate) active perms
        active_perms
    }

    /// Build a new SctxMgr for user threads.
    pub fn new(ctx: SecurityContextRef, thid: u64) -> Self {
        Self {
            inner: Spinlock::new(SecCtxMgrInner {
                active: ctx,
                inactive: Default::default(),
            }),
            thid,
        }
    }

    /// Build a new SctxMgr for kernel threads.
    pub fn new_kernel() -> Self {
        Self {
            inner: Spinlock::new(SecCtxMgrInner {
                active: Arc::new(SecurityContext::new(None)),
                inactive: Default::default(),
            }),
            thid: 0,
        }
    }

    pub fn inherit(other: &SecCtxMgr, thid: u64) -> Self {
        let other_inner = other.inner.lock();
        let mut inactive = FnvIndexMap::new();
        for (k, v) in other_inner.inactive.iter() {
            inactive.insert(*k, v.clone()).unwrap();
        }
        Self {
            inner: Spinlock::new(SecCtxMgrInner {
                active: other_inner.active.clone(),
                inactive,
            }),
            thid,
        }
    }

    /// Switch to the specified context.
    #[track_caller]
    pub fn switch_context(&self, id: ObjID) -> SwitchResult {
        let mut inner = self.inner.lock();
        if inner.active.id() == id {
            return SwitchResult::NoSwitch;
        }
        if let Some(mut ctx) = inner.inactive.remove(&id) {
            core::mem::swap(&mut ctx, &mut inner.active);
            // ctx now holds the old active context
            inner.inactive.insert(ctx.id(), ctx).unwrap();
            drop(inner);
            current_memory_context().map(|mc| mc.switch_to(id));
            SwitchResult::Switched
        } else {
            logln!("NOT ATTACHED {}", core::panic::Location::caller());
            drop(inner);
            self.list_all();
            SwitchResult::NotAttached
        }
    }

    /// Attach a security context.
    pub fn attach(&self, sctx: SecurityContextRef) -> twizzler_rt_abi::Result<()> {
        let mut inner = self.inner.lock();
        if inner.active.id() == sctx.id() || inner.inactive.contains_key(&sctx.id()) {
            return Err(NamingError::AlreadyBound.into());
        }
        inner.inactive.insert(sctx.id(), sctx).unwrap();
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
    use twizzler_security::{Cap, SigningKey, SigningScheme, MAX_KEY_SIZE};

    use crate::{random::getrandom, utils::benchmark};
    #[kernel_test]
    fn bench_capability_verification() {
        let mut rand_bytes = [0; MAX_KEY_SIZE];

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
