use tracing::trace;

use crate::{
    library::Library,
    symbol::{LookupFlags, RelocatedSymbol},
    DynlinkError, DynlinkErrorKind,
};

use super::{engine::ContextEngine, Context, LoadedOrUnloaded};

impl<Engine: ContextEngine> Context<Engine> {
    pub fn lookup_symbol<'a>(
        &'a self,
        start: &'a Library<Engine::Backing>,
        name: &str,
        lookup_flags: LookupFlags,
    ) -> Result<RelocatedSymbol<'a, Engine::Backing>, DynlinkError> {
        // First try looking up within ourselves.
        if !lookup_flags.contains(LookupFlags::SKIP_SELF) {
            if let Ok(sym) = start.lookup_symbol(name) {
                return Ok(sym);
            }
        }

        // Next, try all of our transitive dependencies.
        if !lookup_flags.contains(LookupFlags::SKIP_DEPS) {
            let mut visit = petgraph::visit::Bfs::new(&self.library_deps, start.idx);
            while let Some(node) = visit.next(&self.library_deps) {
                let dep = &self.library_deps[node];

                if node != start.idx {
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
