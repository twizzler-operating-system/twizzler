//! Management of global context.

use std::collections::HashMap;

use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use tracing::trace;

use crate::{
    compartment::Compartment,
    library::{BackingData, CtorInfo, InitState, Library, UnloadedLibrary},
    symbol::{LookupFlags, RelocatedSymbol},
    tls::TlsRegion,
    DynlinkError,
};

use self::engine::ContextEngine;

mod deps;
pub mod engine;
mod load;
mod relocate;

#[derive(Default, Clone)]
pub struct Context<Engine: ContextEngine> {
    pub(crate) engine: Engine,
    // Track all the compartment names.
    compartment_names: HashMap<String, Compartment<Engine::Backing>>,

    // We care about both names and dependency ordering for libraries.
    pub(crate) library_names: HashMap<String, NodeIndex>,
    pub(crate) library_deps: StableDiGraph<LoadedOrUnloaded<Engine::Backing>, ()>,

    pub(crate) static_ctors: Vec<CtorInfo>,
}

pub enum LoadedOrUnloaded<Backing: BackingData> {
    Unloaded(UnloadedLibrary),
    Loaded(Library<Backing>),
}

#[allow(dead_code)]
impl<Engine: ContextEngine> Context<Engine> {
    pub fn get_compartment(&self) -> Compartment<Engine::Backing> {
        // TODO
        self.compartment_names.values().nth(0).cloned().unwrap()
    }

    /// Lookup a library by name
    pub fn lookup_library(&self, name: &str) -> Option<&Library<Engine::Backing>> {
        self.library_deps[self.library_names.get(name)?]
    }

    pub fn with_dfs_postorder<'a, I>(
        &self,
        root: &Library<Engine::Backing>,
        mut f: impl FnMut(&LoadedOrUnloaded<Engine::Backing>),
    ) {
        let mut visit = petgraph::visit::DfsPostOrder::new(&self.library_deps, root.idx);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            f(dep)
        }
    }

    pub fn with_bfs<'a, I>(
        &self,
        root: &Library<Engine::Backing>,
        mut f: impl FnMut(&LoadedOrUnloaded<Engine::Backing>),
    ) {
        let mut visit = petgraph::visit::Bfs::new(&self.library_deps, root.idx);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            f(dep)
        }
    }

    pub(crate) fn add_library(
        &mut self,
        compartment: &Compartment<Engine::Backing>,
        lib: UnloadedLibrary,
    ) -> NodeIndex {
        let idx = self.library_deps.add_node(LoadedOrUnloaded::Unloaded(lib));
        self.library_names.insert(lib.name.clone(), idx);
        idx
    }

    pub(crate) fn add_dep(
        &mut self,
        parent: Option<&Library<Engine::Backing>>,
        dep: NodeIndex,
    ) -> NodeIndex {
        self.library_deps.add_edge(parent.idx, dep, ());
    }

    /// Add all dependency edges for a library.
    pub(crate) fn set_lib_deps<'a>(
        &'a mut self,
        lib: &Library<Engine::Backing>,
        deps: impl IntoIterator<Item = &'a Library<Engine::Backing>>,
    ) {
        for dep in deps.into_iter() {
            self.library_deps.add_edge(lib.idx, dep.idx, ());
        }
    }

    pub(crate) fn lookup_symbol(
        &self,
        start: &Library<Engine::Backing>,
        name: &str,
        lookup_flags: LookupFlags,
    ) -> Result<RelocatedSymbol, DynlinkError> {
        // First try looking up within ourselves.
        if !lookup_flags.contains(LookupFlags::SKIP_SELF) {
            if let Ok(sym) = start.lookup_symbol(name) {
                return Ok(sym);
            }
        }

        // Next, try all of our transitive dependencies.
        if !lookup_flags.contains(LookupFlags::SKIP_DEPS) {
            if let Some(sym) = self
                .library_deps
                .neighbors_directed(start.idx.get().unwrap(), petgraph::Direction::Outgoing)
                .find_map(|depidx| {
                    let dep = &self.library_deps[depidx];
                    if depidx != start.idx.get().unwrap() {
                        self.lookup_symbol(dep, name, lookup_flags).ok()
                    } else {
                        None
                    }
                })
            {
                return Ok(sym);
            }
        }

        // Fall back to global search.
        if !lookup_flags.contains(LookupFlags::SKIP_GLOBAL) {
            trace!("falling back to global search for {}", name);
            self.lookup_symbol_global(name)
        } else {
            Err(DynlinkError::NotFound {
                name: name.to_string(),
            })
        }
    }

    pub(crate) fn lookup_symbol_global(&self, name: &str) -> Result<RelocatedSymbol, DynlinkError> {
        for idx in self.library_deps.node_indices() {
            let dep = &self.library_deps[idx];
            if let Ok(sym) = dep.lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(DynlinkError::NotFound {
            name: name.to_string(),
        })
    }

    fn build_ctors(
        &mut self,
        root: &Library<Engine::Backing>,
    ) -> Result<&[CtorInfo], DynlinkError> {
        let mut ctors = vec![];
        self.with_dfs_postorder(root, |lib| {
            if lib.try_set_init_state(InitState::Uninit, InitState::StaticUninit) {
                ctors.push(lib.get_ctor_info())
            }
        });
        let mut ctors = ctors.into_iter().ecollect::<Vec<_>>()?;
        self.static_ctors.append(&mut ctors);
        Ok(&self.static_ctors)
    }

    pub(crate) fn build_runtime_info(
        &mut self,
        root: &Library<Engine::Backing>,
        tls: TlsRegion,
    ) -> Result<RuntimeInitInfo, DynlinkError> {
        let root_name = root.name.clone();
        let ctors = {
            let ctors = self.build_ctors(root)?;
            (ctors.as_ptr(), ctors.len())
        };
        let mut used_slots = vec![];
        self.with_bfs(root, |lib| used_slots.append(&mut lib.used_slots()));
        Ok(RuntimeInitInfo::new(
            ctors.0, ctors.1, tls, self, root_name, used_slots,
        ))
    }

    /// Create a new compartment with a given name.
    pub fn add_compartment(
        &self,
        name: impl ToString,
        root: UnloadedLibrary,
    ) -> Result<Compartment<Engine::Backing>, DynlinkError> {
        todo!()
    }

    /// Iterate through all libraries and process relocations for any libraries that haven't yet been relocated.
    pub fn relocate_all(&self, root: &Library<Engine::Backing>) -> Result<(), DynlinkError> {
        /*
        let inner = self.inner.lock()?;

        roots
            .into_iter()
            .map(|root| {
                debug!("relocate_all: relocation root: {}", root);
                root.relocate(&inner).map(|_| root)
            })
            .ecollect()
        */
        todo!()
    }
}

#[repr(C)]
pub struct RuntimeInitInfo {
    ctor_info_array: *const CtorInfo,
    ctor_info_array_len: usize,

    pub tls_region: TlsRegion,
    pub ctx: *const u8,
    pub root_names: Vec<String>,
    pub used_slots: Vec<usize>,
}

unsafe impl Send for RuntimeInitInfo {}
unsafe impl Sync for RuntimeInitInfo {}

impl RuntimeInitInfo {
    pub(crate) fn new<E: ContextEngine>(
        ctor_info_array: *const CtorInfo,
        ctor_info_array_len: usize,
        tls_region: TlsRegion,
        ctx: &Context<E>,
        root_names: Vec<String>,
        used_slots: Vec<usize>,
    ) -> Self {
        Self {
            ctor_info_array,
            ctor_info_array_len,
            tls_region,
            ctx: ctx as *const _ as *const u8,
            root_names,
            used_slots,
        }
    }

    pub fn ctor_infos(&self) -> &[CtorInfo] {
        unsafe { core::slice::from_raw_parts(self.ctor_info_array, self.ctor_info_array_len) }
    }
}
