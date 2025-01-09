use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};

use crate::{Cap, Del, ObjectId, Permissions};

pub struct SecCtx {
    // lowkey i have no idea what this is, maybe the object
    // the ctx is contained in? daniel said he was working on that.
    // obj: Option<KernelObject<()>>,
    caps: BTreeMap<ObjectId, Vec<Cap>>,
    dels: BTreeMap<ObjectId, Vec<Del>>,
}
struct SecCtxMgr {
    //TODO: inner: Mutex<SecCtx>,
    inner: SecCtxMgrInner,
    // the active security context id? how is this helpful? idk, also spinlock is behind the kernel
    // mutex is also behind kernel, maybe we could make a sync crate?
    // active_id: SpinLock<ObjectId>,
}

pub type SecCtxRef = Arc<SecCtx>;

struct SecCtxMgrInner {
    /// the active security context, add this later
    // active: Mutex<SecCtxRef>,
    active: SecCtxRef,
    // objectid here is the id of the security context
    inactive: BTreeMap<ObjectId, SecCtxRef>,
    // inactive: Vec<SecCtxRef>,
}

/// Information about how we want to access an object for perms checking.
#[derive(Clone, Copy)]
pub struct AccessInfo {
    /// The target object we're accessing
    pub target_id: ObjectId,
    /// The way we are accessing the object
    pub access_kind: Permissions,
    /// The object we are executing in
    pub exec_id: Option<ObjectId>,
    /// Offset into the exec object for the instruction pointer
    pub exec_off: usize,
}

impl SecCtx {
    /// Lookup the permission info for an object, and maybe cache it.
    pub fn lookup(&self, _id: ObjectId) -> Permissions {
        let caps = match self.caps.get(&_id) {
            Some(caps) => caps,
            None => {
                return Permissions::empty();
            }
        };
        let mut perm = Permissions::empty();
        for cap in caps {
            //NOTE: like do we verify here?
            // cap.verify_sig(verifying_key)
            perm |= cap.permissions; // union of all permissions granted
        }
        perm
    }

    // lowkey gotta do these later, dont have these types
    // pub fn new(kobj: Option<KernelObject<()>>) -> Self {
    //     Self {
    //         kobj,
    //         cache: Default::default(),
    //     }
    // }

    pub fn id(&self) -> ObjectId {
        // self.kobj
        //     .as_ref()
        //     .map(|kobj| kobj.id())
        //     .unwrap_or(KERNEL_SCTX)
        todo!()
    }
}

impl SecCtxMgr {
    /// Lookup the permission info for an object in the active context, and maybe cache it.
    pub fn lookup(&self, id: ObjectId) -> Permissions {
        // *self.inner.lock().active.lookup(id)
        todo!()
    }

    /// Get the active context.
    pub fn active(&self) -> SecCtxRef {
        // self.inner.lock().active.clone()
        todo!()
    }

    /// Get the active ID. This is faster than active().id() and doesn't allocate memory (and only
    /// uses a spinlock).
    pub fn active_id(&self) -> ObjectId {
        // *self.active_id.lock()
        todo!()
    }

    /// Check access rights in the active context.
    pub fn check_active_access(&self, _access_info: AccessInfo) -> Permissions {
        // what is the difference between lookup and check_active_access

        self.inner.active.lookup(_access_info.target_id)
    }

    /// Search all attached contexts for access.
    pub fn search_access(&self, _access_info: AccessInfo) -> Permissions {
        let active_perms = self.inner.active.lookup(_access_info.target_id);
        if active_perms.is_empty() {
            // do we want to look for the most permissive context or just the first context?
            // here im just doing the first context
            for (_, ctx) in self.inner.inactive.iter() {
                let perms = ctx.lookup(_access_info.target_id);
                if !perms.is_empty() {
                    return perms;
                }
            }
            return Permissions::empty();
        }
        return active_perms;
    }

    /// Build a new SctxMgr for user threads.
    pub fn new(ctx: SecCtxRef) -> Self {
        let id = ctx.id();
        // Self {
        //     inner: Mutex::new(SecCtxMgrInner {
        //         active: ctx,
        //         inactive: Default::default(),
        //     }),
        //     active_id: Spinlock::new(id),
        // }
        todo!()
    }

    /// Build a new SctxMgr for kernel threads.
    pub fn new_kernel() -> Self {
        // Self {
        //     inner: Mutex::new(SecCtxMgrInner {
        //         active: Arc::new(SecurityContext::new(None)),
        //         inactive: Default::default(),
        //     }),
        //     active_id: Spinlock::new(KERNEL_SCTX),
        // }
        todo!()
    }

    /// Switch to the specified context.
    pub fn switch_context(&self, id: ObjectId) -> SwitchResult {
        // dont have some of these primitives yet
        // if *self.active_id.lock() == id {
        //     return SwitchResult::NoSwitch;
        // }

        // let mut inner = self.inner.lock();
        // if let Some(mut ctx) = inner.inactive.remove(&id) {
        //     core::mem::swap(&mut ctx, &mut inner.active);
        //     *self.active_id.lock() = id;
        //     // ctx now holds the old active context
        //     inner.inactive.insert(ctx.id(), ctx);
        //     current_memory_context().map(|mc| mc.switch_to(id));
        //     SwitchResult::Switched
        // } else {
        //     SwitchResult::NotAttached
        // }
        todo!()
    }

    // Attach a security context.
    // dont have all the types here
    // pub fn attach(&self, sctx: SecCtxRef) -> Result<(), SctxAttachError> {
    //     dont have access to these objects yet, ask daniel how to use these outside of kernel src
    //     let mut inner = self.inner.lock();
    //     if inner.active.id() == sctx.id() || inner.inactive.contains_key(&sctx.id()) {
    //         return Err(SctxAttachError::AlreadyAttached);
    //     }
    //     inner.inactive.insert(sctx.id(), sctx);
    //     Ok(())
    // }
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
        // let inner = self.inner.lock().clone();
        // let active_id = inner.active.id();
        // Self {
        //     inner: Mutex::new(inner),
        //     active_id: Spinlock::new(active_id),
        // }
        todo!()
    }
}

//NOTE: impl these later

// struct GlobalSecCtxMgr {
//     contexts: Mutex<BTreeMap<ObjectId, SecurityContextRef>>,
// }

// lazy_static! {
//     static ref GLOBAL_SECCTX_MGR: GlobalSecCtxMgr = GlobalSecCtxMgr {
//         contexts: Default::default()
//     };
// }

// Get a security contexts from the global cache.
// pub fn get_sctx(id: ObjectId) -> Result<SecurityContextRef, SctxAttachError> {
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
