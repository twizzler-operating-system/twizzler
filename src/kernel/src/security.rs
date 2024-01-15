use alloc::collections::BTreeMap;
use twizzler_abi::object::{ObjID, Protections};

use crate::memory::context::KernelObject;

#[derive(Default, Clone)]
pub struct SecCtxMgr {
    active: Option<SecurityContext>,
    inactive: BTreeMap<ObjID, SecurityContext>,
}

#[derive(Clone)]
pub struct SecurityContext {
    kobj: KernelObject<()>,
    cache: BTreeMap<ObjID, PermsInfo>,
}

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
    pub fn lookup(&mut self, _id: ObjID) -> &PermsInfo {
        todo!()
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear()
    }

    pub fn new(kobj: KernelObject<()>) -> Self {
        Self {
            kobj,
            cache: Default::default(),
        }
    }

    pub fn id(&self) -> ObjID {
        self.kobj.id()
    }
}

impl SecCtxMgr {
    /// Lookup the permission info for an object in the active context, and maybe cache it.
    pub fn lookup(&mut self, id: ObjID) -> &PermsInfo {
        if let Some(active) = &mut self.active {
            active.lookup(id)
        } else {
            todo!()
        }
    }

    pub fn check_active_access(&mut self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

    pub fn search_access(&mut self, _access_info: AccessInfo) -> &PermsInfo {
        todo!()
    }

    pub fn switch_context(&mut self, id: ObjID) -> bool {
        if self.active.as_ref().is_some_and(|active| active.id() == id) {
            return false;
        }

        if let Some(ctx) = self.inactive.remove(&id) {
            self.active = Some(ctx);
            true
        } else {
            false
        }
    }
}
