use tracing::trace;

use crate::{
    library::LibraryId,
    symbol::{LookupFlags, RelocatedSymbol},
    DynlinkError, DynlinkErrorKind,
};

use super::{engine::ContextEngine, Context, LoadedOrUnloaded};

impl<Engine: ContextEngine> Context<Engine> {
    /// Search for a symbol, starting from library denoted by start_id. For normal symbol lookup, this should be the
    /// ID of the library that needs a symbol looked up. Flags can be specified which allow control over where to look for the symbol.
    pub fn lookup_symbol<'a>(
        &'a self,
        start_id: LibraryId,
        name: &str,
        lookup_flags: LookupFlags,
    ) -> Result<RelocatedSymbol<'a, Engine::Backing>, DynlinkError> {
        // First try looking up within ourselves.
        if !lookup_flags.contains(LookupFlags::SKIP_SELF) {
            let start_lib = self.get_library(start_id)?;
            if let Ok(sym) = start_lib.lookup_symbol(name) {
                return Ok(sym);
            }
        }

        // Next, try all of our transitive dependencies.
        if !lookup_flags.contains(LookupFlags::SKIP_DEPS) {
            let mut visit = petgraph::visit::Bfs::new(&self.library_deps, start_id.0);
            while let Some(node) = visit.next(&self.library_deps) {
                let dep = &self.library_deps[node];
                if node != start_id.0 {
                    match dep {
                        LoadedOrUnloaded::Unloaded(_) => {}
                        LoadedOrUnloaded::Loaded(dep) => {
                            if let Ok(sym) = dep.lookup_symbol(name) {
                                return Ok(sym);
                            }
                        }
                    }
                }
            }
        }

        // Fall back to global search.
        if !lookup_flags.contains(LookupFlags::SKIP_GLOBAL) {
            trace!("falling back to global search for {}", name);
            self.lookup_symbol_global(name)
        } else {
            Err(DynlinkErrorKind::NameNotFound {
                name: name.to_string(),
            }
            .into())
        }
    }

    pub(crate) fn lookup_symbol_global<'a>(
        &'a self,
        name: &str,
    ) -> Result<RelocatedSymbol<'a, Engine::Backing>, DynlinkError> {
        for idx in self.library_deps.node_indices() {
            let dep = &self.library_deps[idx];
            match dep {
                LoadedOrUnloaded::Unloaded(_) => {}
                LoadedOrUnloaded::Loaded(dep) => {
                    if let Ok(sym) = dep.lookup_symbol(name) {
                        return Ok(sym);
                    }
                }
            }
        }
        Err(DynlinkErrorKind::NameNotFound {
            name: name.to_string(),
        }
        .into())
    }
}
