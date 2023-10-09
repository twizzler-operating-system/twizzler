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
        // Don't load twice!
        if let Some(existing) = ctx.library_names.get(&lib.name) {
            debug!("using existing library for {}", lib.name);
            return Ok(existing.clone());
        }

        debug!("loading library {}", lib);
        // Do this first, since TLS registration and ctor finding needs relocated virtual addresses.
        lib.load(ctx, loader)?;

        lib.register_tls(self)?;

        let ctors = lib.get_ctor_info()?;
        lib.set_ctors(ctors);

        let deps = lib.enumerate_needed(loader)?;
        if !deps.is_empty() {
            debug!("{}: loading {} dependencies", self, deps.len());
        }

        // Insert ourselves into the graph before we load deps, since they may want to point to us.
        let lib = Arc::new(lib);
        ctx.insert_lib_predeps(lib.clone());

        let deps = deps
            .into_iter()
            .map(|lib| self.load_library(lib, ctx, loader))
            .ecollect::<Vec<_>>()?;

        // Finally, add the deps edges.
        ctx.set_lib_deps(&lib, deps);

        Ok(lib)
    }
}
