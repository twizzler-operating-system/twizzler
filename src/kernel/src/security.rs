use alloc::{collections::BTreeMap, sync::Arc};
use twizzler_abi::object::{ObjID, Protections};

use crate::memory::context::KernelObject;

#[derive(Clone)]
pub struct SecCtxMgr {
    active: SecurityContextRef,
    inactive: BTreeMap<ObjID, SecurityContextRef>,
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
    pub fn lookup(&self, id: ObjID) -> &PermsInfo {
        self.active.lookup(id)
    }

    pub fn active(&self) -> &SecurityContextRef {
        &self.active
    }

    pub fn check_active_access(&self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

    pub fn search_access(&self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

    pub fn new(ctx: SecurityContextRef) -> Self {
        Self {
            active: ctx,
            inactive: Default::default(),
        }
    }

    pub fn new_kernel() -> Self {
        Self {
            active: Arc::new(SecurityContext::new(None)),
            inactive: Default::default(),
        }
    }

    pub fn switch_context(&mut self, id: ObjID) -> bool {
        if self.active.id() == id {
            return false;
        }

        if let Some(mut ctx) = self.inactive.remove(&id) {
            core::mem::swap(&mut ctx, &mut self.active);
            // ctx now holds the old active context
            self.inactive.insert(ctx.id(), ctx);
            true
        } else {
            false
        }
    }
}
