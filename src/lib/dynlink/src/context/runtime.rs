use std::alloc::Layout;

use twizzler_abi::object::MAX_SIZE;
use twizzler_rt_abi::core::CtorSet;

use super::{Context, LoadedOrUnloaded};
use crate::{compartment::CompartmentId, library::LibraryId, tls::TlsRegion, DynlinkError};

#[repr(C)]
pub struct RuntimeInitInfo {
    pub tls_region: TlsRegion,
    pub ctx: *const u8,
    pub root_name: String,
    pub used_slots: Vec<usize>,
    pub ctors: Vec<CtorSet>,
    pub bootstrap_alloc_slot: usize,
}

// Safety: the pointers involved here are used for a one-time handoff during bootstrap.
unsafe impl Send for RuntimeInitInfo {}
unsafe impl Sync for RuntimeInitInfo {}

impl RuntimeInitInfo {
    pub(crate) fn new(
        tls_region: TlsRegion,
        ctx: &Context,
        root_name: String,
        ctors: Vec<CtorSet>,
    ) -> Self {
        let alloc_test = unsafe { std::alloc::alloc(Layout::from_size_align(16, 8).unwrap()) }
            as usize
            / MAX_SIZE;
        Self {
            tls_region,
            ctx: ctx as *const _ as *const u8,
            root_name,
            used_slots: vec![],
            ctors,
            bootstrap_alloc_slot: alloc_test,
        }
    }
}

impl Context {
    /// Build up a list of constructors to call for a library and its dependencies.
    pub fn build_ctors_list(
        &self,
        root_id: LibraryId,
        comp: Option<CompartmentId>,
    ) -> Result<Vec<CtorSet>, DynlinkError> {
        let mut ctors = vec![];
        self.with_dfs_postorder(root_id, |lib| match lib {
            LoadedOrUnloaded::Unloaded(_) => {}
            LoadedOrUnloaded::Loaded(lib) => {
                if let Some(comp) = comp {
                    if comp == lib.comp_id {
                        ctors.push(lib.ctors);
                    }
                } else {
                    ctors.push(lib.ctors);
                }
            }
        });
        Ok(ctors)
    }

    /// Build the runtime handoff info for bootstrapping the Twizzler runtime.
    pub fn build_runtime_info(
        &self,
        root_id: LibraryId,
        tls: TlsRegion,
    ) -> Result<RuntimeInitInfo, DynlinkError> {
        let ctors = self.build_ctors_list(root_id, None)?;
        Ok(RuntimeInitInfo::new(
            tls,
            self,
            self.get_library(root_id)?.name.to_string(),
            ctors,
        ))
    }
}
