use std::sync::Arc;

use tracing::debug;

use crate::{
    context::ContextInner,
    library::{Library, LibraryLoader, LibraryRef},
    DynlinkError, ECollector,
};

use super::Compartment;

impl Compartment {
    pub(crate) fn load_library(
        &mut self,
        mut lib: Library,
        ctx: &mut ContextInner,
        loader: &mut impl LibraryLoader,
    ) -> Result<LibraryRef, DynlinkError> {
        debug!("loading library {}", lib);

        let deps = lib.enumerate_needed(loader)?;

        let deps = deps
            .into_iter()
            .map(|lib| self.load_library(lib, ctx, loader))
            .ecollect::<Vec<_>>()?;

        lib.load(ctx, loader)?;

        let lib = Arc::new(lib);
        ctx.insert_lib(lib.clone(), deps);
        Ok(lib)
    }
}
