use alloc::{collections::BTreeMap, sync::Arc};
use lazy_static::lazy_static;
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::SctxAttachError,
};

use crate::{
    memory::context::{KernelMemoryContext, KernelObject, ObjectContextInfo},
    mutex::Mutex,
    obj::LookupFlags,
    spinlock::Spinlock,
};

#[derive(Clone)]
pub struct SecCtxMgrInner {
    active: SecurityContextRef,
    inactive: BTreeMap<ObjID, SecurityContextRef>,
}

pub struct SecCtxMgr {
    inner: Mutex<SecCtxMgrInner>,
    active_id: Spinlock<ObjID>,
}

pub struct SecurityContext {
    kobj: Option<KernelObject<()>>,
    cache: BTreeMap<ObjID, PermsInfo>,
}

pub type SecurityContextRef = Arc<SecurityContext>;

pub const KERNEL_SCTX: ObjID = ObjID::new(0);

#[derive(Clone, Copy)]
pub struct PermsInfo {
    ctx: ObjID,
    prot: Protections,
}

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

    pub fn active(&self) -> SecurityContextRef {
        self.inner.lock().active.clone()
    }

    pub fn active_id(&self) -> ObjID {
        *self.active_id.lock()
    }

    pub fn check_active_access(&self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

    pub fn search_access(&self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

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

    pub fn new_kernel() -> Self {
        Self {
            inner: Mutex::new(SecCtxMgrInner {
                active: Arc::new(SecurityContext::new(None)),
                inactive: Default::default(),
            }),
            active_id: Spinlock::new(KERNEL_SCTX),
        }
    }

    pub fn switch_context(&self, id: ObjID) -> SwitchResult {
        if *self.active_id.lock() == id {
            return SwitchResult::NoSwitch;
        }

        let mut inner = self.inner.lock();
        if let Some(mut ctx) = inner.inactive.remove(&id) {
            core::mem::swap(&mut ctx, &mut inner.active);
            // ctx now holds the old active context
            inner.inactive.insert(ctx.id(), ctx);
            SwitchResult::Switched
        } else {
            SwitchResult::NotAttached
        }
    }

    pub fn attach(&self, sctx: SecurityContextRef) {
        self.inner.lock().inactive.insert(sctx.id(), sctx);
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum SwitchResult {
    NoSwitch,
    Switched,
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

// TODO: security context removal
