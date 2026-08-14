use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::AtomicUsize;

use log::{error, trace, warn};
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    syscall::{MapFlags, SctxStats},
};
use twizzler_rt_abi::error::{NamingError, ObjectError};
pub use twizzler_security::PermsInfo;
use twizzler_security::{Cap, CtxMapItemType, SecCtxBase, SecCtxFlags, VerifyingKey};

use crate::{
    memory::context::{
        KernelMemoryContext, KernelObject, KernelObjectHandle, ObjectContextInfo, kernel_context,
        virtmem::with_each_context,
    },
    mutex::Mutex,
    obj::{LookupFlags, LookupResult, lookup_object},
    once::{Once, OnceWait},
    spinlock::Spinlock,
};

#[derive(Clone)]
struct SecCtxMgrInner {
    /// Every context this thread is attached to, the active one included. Which member is active
    /// is tracked separately, so that switching does not have to move entries between two maps --
    /// and so does not have to take this mutex at all when the caller already holds a reference.
    attached: BTreeMap<ObjID, SecurityContextRef>,
}

/// Management of per-thread security context info.
///
/// This is the *attachment* set only. Which member is active lives in the owning thread's
/// `SctxCache`, under the same lock as the rest of the switch state, so that a context switch is
/// one lock acquisition -- and so that reading the active context, which the page-fault path does
/// constantly, never touches this mutex. See [`crate::thread::Thread::active_sctx_id`].
pub struct SecCtxMgr {
    inner: Mutex<SecCtxMgrInner>,
}

/// A single security context.
pub struct SecurityContext {
    kobj: Option<KernelObject<SecCtxBase>>,
    /// Memoized `lookup` answers.
    ///
    /// A `Spinlock`, not a `Mutex`: the page-fault path reads this once per user fault, and a
    /// sleeping-mutex acquire/release pair around a `BTreeMap::get` is most of what the security
    /// stage costs. The miss arm below does all of its real work -- object lookups, kernel object
    /// mapping -- before touching this, so the only thing held under it is the map operation
    /// itself.
    cache: Spinlock<BTreeMap<ObjID, PermsInfo>>,
    attached_count: AtomicUsize,
    active_count: AtomicUsize,
}

impl Drop for SecurityContext {
    fn drop(&mut self) {
        if self.id() == KERNEL_SCTX {
            return;
        }
        with_each_context(|ctx| {
            ctx.unregister_sctx(self.id());
        });
    }
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
#[derive(Clone, Copy, Debug)]
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

    pub fn inc_active_count(&self) {
        self.active_count
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    }

    pub fn dec_active_count(&self) {
        self.active_count
            .fetch_sub(1, core::sync::atomic::Ordering::SeqCst);
    }

    pub fn inc_attached_count(&self) {
        self.attached_count
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    }

    pub fn dec_attached_count(&self) {
        self.attached_count
            .fetch_sub(1, core::sync::atomic::Ordering::SeqCst);
    }

    pub fn active_count(&self) -> usize {
        self.active_count.load(core::sync::atomic::Ordering::SeqCst)
    }

    pub fn attached_count(&self) -> usize {
        self.attached_count
            .load(core::sync::atomic::Ordering::SeqCst)
    }

    /// Lookup the permission info for an object, and maybe cache it.
    pub fn lookup(&self, _id: ObjID, default_prots: Protections) -> PermsInfo {
        // The kernel context has no object to hold capabilities in, and the fault path already
        // grants sctx 0 everything unconditionally (`fault::check_security`). Grant the same here,
        // so an eager map and a faulted-in one compute identical protections -- otherwise
        // `is_object_mapped` rejects the eager entry and the fault path remaps it anyway.
        if self.kobj.is_none() {
            return PermsInfo::new(KERNEL_SCTX, Protections::all(), Protections::empty());
        }
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

            let Some(meta) = target_obj.read_meta() else {
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
                    warn!("ignoring unsupported delegation entry: {entry:#?}");
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
            cache: Spinlock::new(BTreeMap::new()),
            attached_count: AtomicUsize::new(0),
            active_count: AtomicUsize::new(0),
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
    /// Look up an attached context by ID.
    pub fn attached(&self, id: ObjID) -> Option<SecurityContextRef> {
        self.inner.lock().attached.get(&id).cloned()
    }

    /// Search all attached contexts for access, starting from `active`.
    pub fn search_access(
        &self,
        active: &SecurityContextRef,
        access_info: &AccessInfo,
        default_prots: Protections,
    ) -> PermsInfo {
        let active_perms = active.lookup(access_info.target_id, default_prots);

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
        if let Some(flags) = active.flags()
            && flags.contains(SecCtxFlags::UNDETACHABLE)
        {
            trace!("UNDETACHABLE bit set, refusing to evaluate inactive security contexts.");
            return active_perms;
        };

        // Look through the attached contexts to see if any of them match. This includes the active
        // one, which was already checked above; re-checking it costs one cache-hit lookup and is
        // what lets the map stay agnostic about which member is active.
        for (_, ctx) in &self.inner.lock().attached {
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

    /// Build a new SctxMgr for user threads, attached to `ctx`.
    pub fn new(ctx: SecurityContextRef) -> Self {
        let mut attached = BTreeMap::new();
        attached.insert(ctx.id(), ctx);
        Self {
            inner: Mutex::new(SecCtxMgrInner { attached }),
        }
    }

    /// Build a new SctxMgr for kernel threads.
    pub fn new_kernel() -> Self {
        Self::new(kernel_sctx())
    }

    /// Attach a security context.
    pub fn attach(&self, sctx: SecurityContextRef) -> twizzler_rt_abi::Result<()> {
        let mut inner = self.inner.lock();
        if inner.attached.contains_key(&sctx.id()) {
            return Err(NamingError::AlreadyBound.into());
        }
        sctx.inc_attached_count();
        inner.attached.insert(sctx.id(), sctx);
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
        Self {
            inner: Mutex::new(inner),
        }
    }
}

struct GlobalSecCtxMgr {
    contexts: Mutex<BTreeMap<ObjID, SecurityContextRef>>,
}

static GLOBAL_SECCTX_MGR: OnceWait<GlobalSecCtxMgr> = OnceWait::new();

fn global_secctx_mgr() -> &'static GlobalSecCtxMgr {
    GLOBAL_SECCTX_MGR.call_once(|| GlobalSecCtxMgr {
        contexts: Default::default(),
    })
}

pub fn get_sctx_stats() -> SctxStats {
    // Derived here rather than maintained by every context switch. Keeping a per-context counter
    // cost two SeqCst RMWs on a shared cache line per switch -- plus the `Weak::upgrade` of the
    // outgoing context that existed solely to decrement it -- to feed this one stats call. Asking
    // the threads instead puts the cost where it is paid once, and this is the only reader.
    //
    // Counted before the global lock is taken: this walks the thread list and each thread's own
    // sctx-cache lock, and nothing else in the system takes those in that order.
    let mut active_count = 0;
    crate::processor::sched::with_each_thread(|t| {
        if t.active_sctx_id() != KERNEL_SCTX {
            active_count += 1;
        }
    });
    let global = global_secctx_mgr().contexts.lock();
    let mut attached_count = 0;
    for (_, ctx) in global.iter() {
        attached_count += ctx.attached_count();
        // The counter arm of the A/B in `thread::sctx`; when it is maintained it is authoritative
        // and the thread walk above is the approximation, not the other way round.
        active_count = active_count.max(ctx.active_count());
    }
    SctxStats {
        nr_sctx: global.len(),
        nr_active: active_count,
        nr_cached: attached_count,
    }
}

// `Once`, not `OnceWait`: `Thread::new_idle` calls this from `init_threading` on each secondary
// cpu, where there is no current thread yet, and `OnceWait`'s condvar path unwraps one.
static KERNEL_SECCTX: Once<SecurityContextRef> = Once::new();

/// The one security context for [`KERNEL_SCTX`], which is also
/// `MONITOR_INSTANCE_ID` -- the monitor runs as sctx 0.
///
/// There is exactly one, globally: it has no backing object (there is no object 0 to look up), so
/// every `SecurityContext::new(None)` is interchangeable with it, and every `VirtContext`
/// registers an `ArchContext` under `KERNEL_SCTX` at construction. Resolving it rather than
/// failing is what lets `VirtContext::map_object` install page-table entries for sctx-0 mappings
/// -- the monitor's, which is nearly every mapping in the system -- instead of leaving all of them
/// to the fault path (mapperf.md).
pub fn kernel_sctx() -> SecurityContextRef {
    KERNEL_SECCTX
        .call_once(|| Arc::new(SecurityContext::new(None)))
        .clone()
}

/// Get a security contexts from the global cache.
pub fn get_sctx(id: ObjID) -> twizzler_rt_abi::Result<SecurityContextRef> {
    // Not in the global map: it has no object, so the miss arm below cannot build it, and
    // `SecCtxMgr::drop` already refuses to reap it. Note that this makes `sys_sctx_attach(0)`
    // succeed where the failing `lookup_object` used to reject it -- see the guard there.
    if id == KERNEL_SCTX {
        return Ok(kernel_sctx());
    }
    // Hit path: one lock and a clone. `obj` below is used only by the miss arm, so an unconditional
    // `lookup_object` made every hit pay a global object-table lookup for a value it discarded --
    // and this is on the gate-entry path (pagerperf.md 15: `sys_sctx_attach` costs ~30 us "all to
    // conclude the thread is already attached").
    //
    // Checking the cache first is only safe because the miss arm below builds its kernel object
    // *outside* this lock. It previously did so inside `or_insert_with`, and
    // `insert_kernel_object` -> `VirtContext::map_object` -> `get_sctx` is a real recursion; what
    // terminated it was the pre-lock `lookup_object` failing for KERNEL_SCTX, rather than anything
    // deliberate, so reordering alone panics with "this mutex is not re-entrant". The KERNEL_SCTX
    // early return above now terminates that recursion outright, but the hoist stands on its own.
    //
    // Behaviour note: a cached context whose object has since been deleted now returns `Ok` where
    // the lookup would have reported `NoSuchObject`. A cache entry exists only while something
    // holds a reference (`SecCtxMgr::drop` reaps it when the manager and this map are the last
    // two), so the window is narrow -- but this is security-relevant code and the change is real.
    if let Some(entry) = global_secctx_mgr().contexts.lock().get(&id) {
        return Ok(entry.clone());
    }

    let obj =
        crate::obj::lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
    // Built before the lock, for two reasons: it removes the recursion above, and it takes a
    // mapping call out of a global lock that every security check in the system passes through.
    // TODO: use control object cacher.
    let kobj =
        crate::memory::context::kernel_context().insert_kernel_object(ObjectContextInfo::new(
            obj,
            Protections::READ,
            twizzler_abi::device::CacheType::WriteBack,
            MapFlags::empty(),
        ));
    let mut global = global_secctx_mgr().contexts.lock();
    // A thread that lost the race drops its `kobj` unused, which is the cost of not holding the
    // lock across the mapping.
    let entry = global
        .entry(id)
        .or_insert_with(|| Arc::new(SecurityContext::new(Some(kobj))));
    Ok(entry.clone())
}

impl Drop for SecCtxMgr {
    fn drop(&mut self) {
        let mut global = global_secctx_mgr().contexts.lock();
        let inner = self.inner.lock();
        // Check the contexts we have a reference to. If the value is 2, then it's only us and the
        // global mgr that have a ref. Since we hold the global mgr lock, this will not get
        // incremented if no one else holds a ref.
        //
        // The owning thread's `SctxCache` -- including its notion of which context is active --
        // deliberately holds only `Weak`s, so nothing there inflates this count, and this reap does
        // not have to happen in any particular order relative to the cache being dropped.
        for ctx in inner.attached.values() {
            if ctx.id() != KERNEL_SCTX && Arc::strong_count(ctx) == 2 {
                global.remove(&ctx.id());
            }
        }
    }
}

mod tests {
    use core::hint::black_box;

    use twizzler_abi::object::Protections;
    use twizzler_kernel_macros::kernel_test;
    use twizzler_security::{Cap, MAX_KEY_SIZE, SigningKey, SigningScheme};

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
