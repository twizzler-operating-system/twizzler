use petgraph::graph::NodeIndex;

use super::{Context, LoadedOrUnloaded};
use crate::{
    library::{Library, LibraryId},
    symbol::{LookupFlags, RelocatedSymbol},
    DynlinkError, DynlinkErrorKind, Vec,
};

impl Context {
    pub fn build_deps_search_list(&self, start_id: LibraryId) -> Vec<NodeIndex, 32> {
        let mut ret = Vec::<_, 32>::new();
        let mut visit = petgraph::visit::Bfs::new(&self.library_deps, start_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            ret.push(node);
        }
        ret
    }
    /// Search for a symbol, starting from library denoted by start_id. For normal symbol lookup,
    /// this should be the ID of the library that needs a symbol looked up. Flags can be
    /// specified which allow control over where to look for the symbol.
    pub fn lookup_symbol<'a>(
        &'a self,
        start_id: LibraryId,
        name: &str,
        lookup_flags: LookupFlags,
        deps_list: &[NodeIndex],
    ) -> Result<RelocatedSymbol<'a>, DynlinkError> {
        // No timing here: this is the hot path, and Instant::now() is not free (rdtsc plus u128
        // femtosecond conversion). Per-library relocation timing is reported by relocate_single.
        self.do_lookup_symbol(start_id, name, lookup_flags, deps_list)
    }

    fn do_lookup_symbol<'a>(
        &'a self,
        start_id: LibraryId,
        name: &str,
        lookup_flags: LookupFlags,
        deps_list: &[NodeIndex],
    ) -> Result<RelocatedSymbol<'a>, DynlinkError> {
        let allow_weak = lookup_flags.contains(LookupFlags::ALLOW_WEAK);
        let start_lib = self.get_library(start_id)?;
        // Hashed once; every membership filter consulted below shares it. A filter miss proves the
        // library defines neither spelling of the name (`build_sym_bloom` indexes gate-prefixed
        // definitions under their bare name too), so both the hash-table probe and the prefixed
        // retry are skipped. A library without a filter is probed as before.
        let hash = super::sym_hash(name);
        let may_define =
            |lib: &Library| lib.sym_bloom.as_ref().is_none_or(|b| b.maybe_contains(hash));
        // First try looking up within ourselves.
        if !lookup_flags.contains(LookupFlags::SKIP_SELF) && may_define(start_lib) {
            if let Some(sym) = start_lib.lookup_symbol(name, allow_weak, false) {
                return Ok(sym);
            }
        }

        // Next, try all of our transitive dependencies.
        if !lookup_flags.contains(LookupFlags::SKIP_DEPS) {
            for node in deps_list {
                let dep = &self.library_deps[*node];
                if *node != start_id.0 {
                    match dep {
                        LoadedOrUnloaded::Unloaded(_) => {}
                        LoadedOrUnloaded::Loaded(dep) => {
                            tracing::trace!("trying in {}", dep.name);
                            if may_define(dep)
                                && (lookup_flags.contains(LookupFlags::SKIP_SECGATE_CHECK)
                                    || dep.is_local_or_secgate_from(start_lib, name))
                            {
                                let allow_weak =
                                    allow_weak && dep.in_same_compartment_as(start_lib);
                                let try_prefix =
                                    dep.in_same_compartment_as(start_lib) || dep.allows_gates();
                                // A pre-prefixed gate name (a weak-bound gate import) binds only
                                // where the prefixed retry would have run: the trampoline is the
                                // same symbol bare-name matching resolves to, so it must obey the
                                // same gate-export policy.
                                if try_prefix || !crate::library::is_gate_symbol(name) {
                                    if let Some(sym) =
                                        dep.lookup_symbol(name, allow_weak, try_prefix)
                                    {
                                        return Ok(sym);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fall back to global search.
        if !lookup_flags.contains(LookupFlags::SKIP_GLOBAL) {
            tracing::trace!("falling back to global search for {}", name);

            if let Some(sym) = self.lookup_symbol_global(start_lib, name, lookup_flags) {
                return Ok(sym);
            }

            if !allow_weak {
                let res = self.lookup_symbol(
                    start_id,
                    name,
                    lookup_flags.union(LookupFlags::ALLOW_WEAK),
                    deps_list,
                );
                if res.is_ok() {
                    return res;
                }
            }
        }
        Err(DynlinkErrorKind::NameNotFound { name: name.into() }.into())
    }

    pub(crate) fn lookup_symbol_global<'a>(
        &'a self,
        start_lib: &Library,
        name: &str,
        lookup_flags: LookupFlags,
    ) -> Option<RelocatedSymbol<'a>> {
        // Ascending node order, which is the resolution order the name -> instances index existed
        // to preserve ("lowest node index wins"); `node_indices` yields it directly.
        let hash = super::sym_hash(name);
        let skip_secgate_check = lookup_flags.contains(LookupFlags::SKIP_SECGATE_CHECK);
        let start_comp = start_lib.compartment();
        for idx in self.library_deps.node_indices() {
            let LoadedOrUnloaded::Loaded(dep) = &self.library_deps[idx] else {
                continue;
            };
            // Cheap rejects first, cheapest last-to-fail: a library in another compartment can
            // only match via the secgate path, which is impossible if it exports no gates.
            if !skip_secgate_check && dep.compartment() != start_comp && dep.secgate_info.num == 0 {
                continue;
            }
            if dep
                .sym_bloom
                .as_ref()
                .is_some_and(|bloom| !bloom.maybe_contains(hash))
            {
                continue;
            }
            if skip_secgate_check || dep.is_local_or_secgate_from(start_lib, name) {
                let allow_weak = lookup_flags.contains(LookupFlags::ALLOW_WEAK)
                    && dep.in_same_compartment_as(start_lib);
                let try_prefix = (idx != start_lib.id().0 || dep.allows_self_gates())
                    && (dep.allows_gates() || dep.in_same_compartment_as(start_lib));
                // Same rule as the deps loop: a pre-prefixed gate name binds only where the
                // prefixed retry would have run.
                if try_prefix || !crate::library::is_gate_symbol(name) {
                    if let Some(sym) = dep.lookup_symbol(name, allow_weak, try_prefix) {
                        return Some(sym);
                    }
                }
            }
        }
        None
    }
}
