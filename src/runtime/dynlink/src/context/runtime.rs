use crate::{
    library::{CtorInfo, Library, LibraryId},
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
}

unsafe impl Send for RuntimeInitInfo {}
unsafe impl Sync for RuntimeInitInfo {}

impl RuntimeInitInfo {
    pub(crate) fn new<E: ContextEngine>(
        tls_region: TlsRegion,
        ctx: &Context<E>,
        root_name: String,
        ctors: Vec<CtorInfo>,
    ) -> Self {
        Self {
            tls_region,
            ctx: ctx as *const _ as *const u8,
            root_name,
            used_slots: vec![],
            ctors,
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

    pub fn build_runtime_info(
        &self,
        root_id: LibraryId,
        tls: TlsRegion,
    ) -> Result<RuntimeInitInfo, DynlinkError> {
        let ctors = self.build_ctors(root_id)?;
        Ok(RuntimeInitInfo::new(
            tls,
            self,
            self.get_library(root_id)?.name.to_string(),
            ctors,
        ))
    }
}
