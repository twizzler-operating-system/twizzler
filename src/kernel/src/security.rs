use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use log::{error, trace, warn};
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    syscall::MapFlags,
};
use twizzler_rt_abi::error::{NamingError, ObjectError};
pub use twizzler_security::PermsInfo;
use twizzler_security::{
    Cap, CtxMapItemType, Del, MAX_DELEGATION_NEST, SecCtxBase, SecCtxFlags, VerifyingKey,
};

use crate::{
    memory::context::{
        KernelMemoryContext, KernelObject, KernelObjectHandle, ObjectContextInfo, UserContext,
        kernel_context, virtmem::KernelObjectVirtHandle,
    },
    mutex::Mutex,
    obj::{LookupFlags, LookupResult, lookup_object},
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
    pub fn flags(&self) -> Option<SecCtxFlags> {
        let obj = self.kobj.as_ref()?;
        let base = obj.base();
        Some(base.flags.clone())
    }

    /// Return the maximum allowed access for this object id that
    /// is adherent to the compossibilities in this context.
    pub fn compossibility_check(&self, id: ObjID) -> Protections {
        let Some(ref obj) = self.kobj else {
            // we dont care about restricting if we dont have a underlying secuirty context.
            return Protections::all();
        };
        let ctx = obj.base();

        // collect all the offsets for everything that's a del into a vector
        let del_offsets: Vec<usize> = ctx
            .map
            .values()
            .flat_map(|entries| entries.iter())
            .filter(|entry| matches!(entry.item_type, CtxMapItemType::Del))
            .map(|entry| entry.offset)
            .collect();

        let mut running_prots = Protections::all();

        for offset in del_offsets {
            let Some(del) = obj.lea_raw(offset as *const Del) else {
                error!(
                    "Unable to cast offset into delegation. No protections granted. Obj: {id:#?}, offset: {offset}"
                );
                return Protections::empty();
            };

            for com in &del.compossibilities {
                // we want the running prots to be and'd with c_mask if the compossibility applies
                // to it, and if it doesn't it gets the gc_mask.
                running_prots &= if id == com.target {
                    com.c_mask
                } else {
                    com.gcmask
                };
            }
        }
        running_prots
    }

    /// Lookup the permission info for an object, and maybe cache it.
    pub fn lookup(&self, id: ObjID, default_prots: Protections) -> PermsInfo {
        // check the cache to see if we already have something
        if let Some(cache_entry) = self.cache.lock().get(&id) {
            return *cache_entry;
        }

        let mut granted_perms = PermsInfo::new(
            self.id(),
            Protections::empty(),
            // we want to restrict the granted permissions to the maximum allowed by all the
            // compossibilities in this security context.
            self.compossibility_check(id).complement(),
        );

        // add default perms here
        granted_perms.provide = granted_perms.provide | default_prots;

        let Some(ref obj) = self.kobj else {
            // if there is no object underneath the kobj, return nothing;
            return granted_perms;
        };

        let base = obj.base();

        // iterate through

        // check for possible items
        let Some(results) = base.map.get(&id) else {
            // if there arent any items inside this context, just return default perms
            return granted_perms;
        };

        // from now on, whenever we return granted_perms, it must be &'d with the sec_ctx global
        // mask, since there are some entries inside the base.map()

        let Some(v_obj) = fetch_verifying_key_from_obj_id(id) else {
            granted_perms.provide &= base.global_mask;
            return granted_perms;
        };

        let v_key = v_obj.base();

        for entry in results {
            match entry.item_type {
                CtxMapItemType::Del => {
                    let Some(del) = obj.lea_raw(entry.offset as *const Del) else {
                        error!("Failed to map delegation from entry: {entry:#?}");
                        // something weird going on, entry offset not inside object bounds,
                        // return already granted perms to avoid panic
                        granted_perms.provide &= base.global_mask;
                        return granted_perms;
                    };

                    let Some(provider_ctx_v_obj) = fetch_verifying_key_from_obj_id(del.provider)
                    else {
                        // what happens if your provider security context has no
                        // verifying key, meaning it was created as a bare object
                        // how about we just say thats impossible?
                        //TODO: need to ask owen about the stuff here
                        granted_perms.provide &= base.global_mask;
                        return granted_perms;
                    };

                    let provider_ctx_v_key = provider_ctx_v_obj.base();

                    if del.verify_sig(provider_ctx_v_key).is_err() || del.target != id {
                        warn!("Signature invalid for del: {del:#?}, moving on to next entry");
                        continue;
                    }

                    let mut mask = del.prot_mask;
                    let mut provider = del.provider;
                    let mut item_type = del.inner.item_type;
                    let mut offset = del.inner.offset;
                    let mut resolved = None;

                    for _ in 0..MAX_DELEGATION_NEST {
                        let provider_obj = match lookup_object(provider, LookupFlags::empty()) {
                            LookupResult::Found(o) => o,
                            _ => break,
                        };

                        let k_ctx = kernel_context();
                        let provider_kobj =
                            k_ctx.insert_kernel_object::<SecCtxBase>(ObjectContextInfo::new(
                                provider_obj,
                                Protections::READ,
                                CacheType::WriteBack,
                                MapFlags::STABLE,
                            ));

                        match item_type {
                            CtxMapItemType::Cap => {
                                let Some(cap) = provider_kobj.lea_raw(offset as *const Cap) else {
                                    break;
                                };
                                if cap.verify_sig(v_key).is_ok() && cap.target == id {
                                    resolved = Some(cap.prots);
                                }
                                break;
                            }
                            CtxMapItemType::Del => {
                                let Some(next_del) = provider_kobj.lea_raw(offset as *const Del)
                                else {
                                    break;
                                };

                                let Some(provider_ctx_v_obj) =
                                    fetch_verifying_key_from_obj_id(del.provider)
                                else {
                                    // what happens if your provider security context has no
                                    // verifying key, meaning it was created as a bare object
                                    // how about we just say thats impossible?
                                    //TODO: need to ask owen about the stuff here
                                    granted_perms.provide &= base.global_mask;
                                    return granted_perms;
                                };

                                let provider_ctx_v_key = provider_ctx_v_obj.base();

                                if next_del.verify_sig(provider_ctx_v_key).is_err()
                                    || next_del.target != id
                                {
                                    break;
                                }
                                mask &= next_del.prot_mask;
                                provider = next_del.provider;
                                item_type = next_del.inner.item_type;
                                offset = next_del.inner.offset;
                            }
                        }
                    }

                    if let Some(prots) = resolved {
                        granted_perms.provide |= prots & mask;
                    }
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
                        granted_perms.provide |= cap.prots;
                    };
                }
            }
        }

        // lookup mask for obj in base
        let Some(mask) = base.masks.get(&id) else {
            // no mask for target object
            // final perms are granted_perms & global_mask
            granted_perms.provide &= base.global_mask;
            self.cache.lock().insert(id, granted_perms.clone());
            return granted_perms;
        };

        // final permissions will be:
        // granted_perms & permmask & (global_mask | override_mask)
        granted_perms.provide =
            granted_perms.provide & mask.permmask & (base.global_mask | mask.ovrmask);
        self.cache.lock().insert(id, granted_perms.clone());
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
            current_memory_context().map(|mc| mc.switch_to(id));
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

fn fetch_verifying_key_from_obj_id(id: ObjID) -> Option<KernelObjectVirtHandle<VerifyingKey>> {
    let obj = match lookup_object(id, LookupFlags::empty()) {
        LookupResult::Found(obj) => obj,
        _ => return None,
    };

    let Some(meta) = obj.read_meta(true) else {
        return None;
    };

    match lookup_object(meta.kuid, LookupFlags::empty()) {
        LookupResult::Found(v_obj) => {
            let k_ctx = kernel_context();
            let handle = k_ctx.insert_kernel_object::<VerifyingKey>(ObjectContextInfo::new(
                v_obj,
                Protections::READ,
                CacheType::WriteBack,
                MapFlags::STABLE,
            ));
            Some(handle)
        }
        _ => {
            return None;
        }
    }
}

mod tests {
    use core::hint::black_box;

    use twizzler_abi::object::Protections;
    use twizzler_kernel_macros::kernel_test;
    use twizzler_security::{CapBuilder, MAX_KEY_SIZE, SigningKey, SigningScheme};

    use crate::{random::getrandom, utils::benchmark};
    #[kernel_test]
    fn bench_capability_verification() {
        let mut rand_bytes = [0; MAX_KEY_SIZE];

        getrandom(&mut rand_bytes, false);

        let (s_key, v_key) = SigningKey::new_kernel_keypair(&SigningScheme::Ecdsa, rand_bytes)
            .expect("shouldnt have errored");

        let cap = CapBuilder::new(0x123.into(), 0x100.into())
            .protections(Protections::all())
            .build(&s_key)
            .expect("capability creation shouldnt have errored");

        benchmark(|| {
            let _x = black_box(cap.verify_sig(&v_key).expect("should succeed"));
        });
    }

    //TODO: write a thorough security context test when that stuff is implemented
}
