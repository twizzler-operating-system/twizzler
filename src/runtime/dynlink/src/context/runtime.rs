use crate::{
    library::{CtorInfo, LibraryId},
    tls::TlsRegion,
    DynlinkError,
};

use super::{engine::ContextEngine, Context, LoadedOrUnloaded};

#[repr(C)]
pub struct RuntimeInitInfo {
    pub tls_region: TlsRegion,
    pub ctx: *const u8,
    pub root_name: String,
    pub used_slots: Vec<usize>,
    pub ctors: Vec<CtorInfo>,
    pub flags: RuntimeInitFlags,
}

bitflags::bitflags! {
    pub struct RuntimeInitFlags: u32 {
        const IS_MONITOR = 1;
    }
}

// Safety: the pointers involved here are used for a one-time handoff during bootstrap.
unsafe impl Send for RuntimeInitInfo {}
unsafe impl Sync for RuntimeInitInfo {}

impl RuntimeInitInfo {
    pub(crate) fn new<E: ContextEngine>(
        tls_region: TlsRegion,
        ctx: &Context<E>,
        root_name: String,
        ctors: Vec<CtorInfo>,
        flags: RuntimeInitFlags,
    ) -> Self {
        Self {
            tls_region,
            ctx: ctx as *const _ as *const u8,
            root_name,
            used_slots: vec![],
            ctors,
            flags,
        }
    }
}

impl<Engine: ContextEngine> Context<Engine> {
    fn build_ctors(&self, root_id: LibraryId) -> Result<Vec<CtorInfo>, DynlinkError> {
        let mut ctors = vec![];
        self.with_dfs_postorder(root_id, |lib| match lib {
            LoadedOrUnloaded::Unloaded(_) => {}
            LoadedOrUnloaded::Loaded(lib) => {
                ctors.push(lib.ctors);
            }
        });
        Ok(ctors)
    }

    /// Build the runtime handoff info for bootstrapping the Twizzler runtime.
    pub fn build_runtime_info(
        &self,
        root_id: LibraryId,
        tls: TlsRegion,
        flags: RuntimeInitFlags,
    ) -> Result<RuntimeInitInfo, DynlinkError> {
        let ctors = self.build_ctors(root_id)?;
        Ok(RuntimeInitInfo::new(
            tls,
            self,
            self.get_library(root_id)?.name.to_string(),
            ctors,
            flags,
        ))
    }
}
