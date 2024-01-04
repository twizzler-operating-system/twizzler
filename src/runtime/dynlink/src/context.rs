//! Management of global context.

use std::collections::HashMap;

use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use tracing::trace;

use crate::DynlinkErrorKind;
use crate::{
    compartment::Compartment,
    library::{BackingData, CtorInfo, Library, UnloadedLibrary},
    symbol::{LookupFlags, RelocatedSymbol},
    tls::TlsRegion,
    DynlinkError,
};

use self::engine::ContextEngine;

mod deps;
pub mod engine;
mod load;
mod relocate;

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

impl<Backing: BackingData> LoadedOrUnloaded<Backing> {
    pub fn name(&self) -> &str {
        match self {
            LoadedOrUnloaded::Unloaded(unlib) => &unlib.name,
            LoadedOrUnloaded::Loaded(lib) => &lib.name,
        }
    }
}

#[allow(dead_code)]
impl<Engine: ContextEngine> Context<Engine> {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            compartment_names: HashMap::new(),
            library_names: HashMap::new(),
            library_deps: StableDiGraph::new(),
            static_ctors: Vec::new(),
        }
    }

    pub fn get_compartment(&self, name: &str) -> Option<&Compartment<Engine::Backing>> {
        self.compartment_names.get(name)
    }

    /// Lookup a library by name
    pub fn lookup_library(&self, name: &str) -> Option<&LoadedOrUnloaded<Engine::Backing>> {
        Some(&self.library_deps[*self.library_names.get(name)?])
    }

    pub fn with_dfs_postorder(
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

    pub fn with_bfs(
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
        let name = lib.name.clone();
        let idx = self.library_deps.add_node(LoadedOrUnloaded::Unloaded(lib));
        self.library_names.insert(name, idx);
        idx
    }

    pub fn load_library_in_compartment<N>(
        &mut self,
        compartment: &mut Compartment<Engine::Backing>,
        unlib: UnloadedLibrary,
        n: N,
    ) -> Result<&Library<Engine::Backing>, DynlinkError>
    where
        N: FnMut(&str) -> Option<Engine::Backing> + Clone,
    {
        let idx = self.add_library(compartment, unlib.clone());
        let idx = self.load_library(compartment, unlib.clone(), idx, n)?;
        match &self.library_deps[idx] {
            LoadedOrUnloaded::Unloaded(_) => {
                Err(DynlinkErrorKind::LibraryLoadFail { library: unlib }.into())
            }
            LoadedOrUnloaded::Loaded(lib) => Ok(lib),
        }
    }

    pub(crate) fn add_dep(&mut self, parent: &Library<Engine::Backing>, dep: NodeIndex) {
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
            if let Some(sym) = self
                .library_deps
                .neighbors_directed(start.idx, petgraph::Direction::Outgoing)
                .find_map(|depidx| {
                    let dep = &self.library_deps[depidx];
                    if depidx != start.idx {
                        match dep {
                            LoadedOrUnloaded::Unloaded(_) => None,
                            LoadedOrUnloaded::Loaded(dep) => {
                                self.lookup_symbol(dep, name, lookup_flags).ok()
                            }
                        }
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

    fn build_ctors(
        &mut self,
        root: &Library<Engine::Backing>,
    ) -> Result<&[CtorInfo], DynlinkError> {
        let mut ctors = vec![];
        self.with_dfs_postorder(root, |lib| match lib {
            LoadedOrUnloaded::Unloaded(_) => {}
            LoadedOrUnloaded::Loaded(lib) => {
                ctors.push(lib.ctors);
            }
        });
        self.static_ctors.append(&mut ctors);
        Ok(&self.static_ctors)
    }

    pub fn build_runtime_info(
        &mut self,
        root: &Library<Engine::Backing>,
        tls: TlsRegion,
    ) -> Result<RuntimeInitInfo, DynlinkError> {
        let root_name = root.name.clone();
        let ctors = {
            let ctors = self.build_ctors(root)?;
            (ctors.as_ptr(), ctors.len())
        };
        Ok(RuntimeInitInfo::new(ctors.0, ctors.1, tls, self, root_name))
    }

    /// Create a new compartment with a given name.
    pub fn add_compartment(
        &mut self,
        name: impl ToString,
    ) -> Result<&Compartment<Engine::Backing>, DynlinkError> {
        let comp = Compartment::new(name.to_string());
        self.compartment_names.insert(comp.name.clone(), comp);
        Ok(&self.compartment_names[&name.to_string()])
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
    pub root_name: String,
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
        root_name: String,
    ) -> Self {
        Self {
            ctor_info_array,
            ctor_info_array_len,
            tls_region,
            ctx: ctx as *const _ as *const u8,
            root_name,
            used_slots: vec![],
        }
    }

    pub fn ctor_infos(&self) -> &[CtorInfo] {
        unsafe { core::slice::from_raw_parts(self.ctor_info_array, self.ctor_info_array_len) }
    }
}
