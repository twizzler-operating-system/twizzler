use std::sync::Arc;

use tracing::debug;

use crate::{
    context::ContextInner,
    library::{Library, LibraryLoader, LibraryRef},
    DynlinkError, ECollector,
};

use super::{Compartment, CompartmentRef};

impl Compartment {
    pub(crate) fn load_library(
        self: &CompartmentRef,
        mut lib: Library,
        ctx: &mut ContextInner,
        loader: &mut impl LibraryLoader,
    ) -> Result<LibraryRef, DynlinkError> {
        if let Some(existing) = ctx.library_names.get(&lib.name) {
            debug!("using existing library for {}", lib.name);
            return Ok(existing.clone());
        }

        debug!("loading library {}", lib);
        lib.load(ctx, loader)?;

        lib.register_tls(self)?;

        let deps = lib.enumerate_needed(loader)?;
        if !deps.is_empty() {
            debug!("{}: loading {} dependencies", self, deps.len());
        }

        let deps = deps
            .into_iter()
            .map(|lib| self.load_library(lib, ctx, loader))
            .ecollect::<Vec<_>>()?;

        let ctors = lib.get_ctor_info()?;
        lib.set_ctors(ctors);

        let lib = Arc::new(lib);
        ctx.insert_lib(lib.clone(), deps);

        Ok(lib)
    }
}
