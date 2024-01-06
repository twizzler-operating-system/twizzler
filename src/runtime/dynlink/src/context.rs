//! Management of global context.

use std::collections::HashMap;
use std::fmt::Display;

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

    // This is the primary list of libraries, all libraries have an entry here, and they are
    // placed here independent of compartment.
    pub(crate) library_deps: StableDiGraph<LoadedOrUnloaded<Engine::Backing>, ()>,
}

// Libraries in the dependency graph are placed there before loading, so that they can participate
// in dependency search. So we need to track both kinds of libraries that may be at a given index in the graph.
pub enum LoadedOrUnloaded<Backing: BackingData> {
    Unloaded(UnloadedLibrary),
    Loaded(Library<Backing>),
}

impl<Backing: BackingData> Display for LoadedOrUnloaded<Backing> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadedOrUnloaded::Unloaded(unlib) => write!(f, "(unloaded){}", unlib),
            LoadedOrUnloaded::Loaded(lib) => write!(f, "(loaded){}", lib),
        }
    }
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
            library_deps: StableDiGraph::new(),
        }
    }

    pub fn get_compartment(&self, name: &str) -> Option<&Compartment<Engine::Backing>> {
        self.compartment_names.get(name)
    }

    pub fn get_compartment_mut(&mut self, name: &str) -> Option<&mut Compartment<Engine::Backing>> {
        self.compartment_names.get_mut(name)
    }

    /// Lookup a library by name
    pub fn lookup_library(
        &self,
        comp: &Compartment<Engine::Backing>,
        name: &str,
    ) -> Option<&LoadedOrUnloaded<Engine::Backing>> {
        Some(&self.library_deps[*comp.library_names.get(name)?])
    }

    pub fn lookup_loaded_library(
        &self,
        comp: &Compartment<Engine::Backing>,
        name: &str,
    ) -> Result<&Library<Engine::Backing>, DynlinkError> {
        match self.lookup_library(comp, name) {
            Some(LoadedOrUnloaded::Loaded(lib)) => Ok(lib),
            Some(LoadedOrUnloaded::Unloaded(unlib)) => {
                Err(DynlinkError::new(DynlinkErrorKind::LibraryLoadFail {
                    library: unlib.clone(),
                }))
            }
            _ => Err(DynlinkError::new(DynlinkErrorKind::NameNotFound {
                name: name.to_string(),
            })),
        }
    }

    pub fn with_dfs_postorder<R>(
        &self,
        root: &Library<Engine::Backing>,
        mut f: impl FnMut(&LoadedOrUnloaded<Engine::Backing>) -> R,
    ) -> Vec<R> {
        let mut rets = vec![];
        let mut visit = petgraph::visit::DfsPostOrder::new(&self.library_deps, root.idx);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            rets.push(f(dep))
        }
        rets
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

    pub(crate) fn add_library(&mut self, lib: UnloadedLibrary) -> NodeIndex {
        self.library_deps.add_node(LoadedOrUnloaded::Unloaded(lib))
    }

    /// Load a library into a given compartment. The namer callback resolves names to Backing objects.
    pub fn load_library_in_compartment<N>(
        &mut self,
        compartment_name: &str,
        unlib: UnloadedLibrary,
        namer: N,
    ) -> Result<&Library<Engine::Backing>, DynlinkError>
    where
        N: FnMut(&str) -> Option<Engine::Backing> + Clone,
    {
        let idx = self.add_library(unlib.clone());
        // Step 1: insert into the compartment's library names.
        let comp = self.get_compartment_mut(compartment_name).ok_or_else(|| {
            DynlinkErrorKind::NameNotFound {
                name: compartment_name.to_string(),
            }
        })?;

        // At this level, it's an error to insert an already loaded library.
        if comp.library_names.contains_key(&unlib.name) {
            return Err(DynlinkErrorKind::NameAlreadyExists {
                name: unlib.name.clone(),
            }
            .into());
        }
        comp.library_names.insert(unlib.name.clone(), idx);

        // Step 2: load the library. This call recurses on dependencies.
        let idx = self.load_library(compartment_name, unlib.clone(), idx, namer)?;
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

    fn build_ctors(&self, root: &Library<Engine::Backing>) -> Result<Vec<CtorInfo>, DynlinkError> {
        let mut ctors = vec![];
        self.with_dfs_postorder(root, |lib| match lib {
            LoadedOrUnloaded::Unloaded(_) => {}
            LoadedOrUnloaded::Loaded(lib) => {
                ctors.push(lib.ctors);
            }
        });
        Ok(ctors)
    }

    pub fn build_runtime_info(
        &self,
        root: &Library<Engine::Backing>,
        tls: TlsRegion,
    ) -> Result<RuntimeInitInfo, DynlinkError> {
        let root_name = root.name.clone();
        let ctors = self.build_ctors(root)?;
        Ok(RuntimeInitInfo::new(tls, self, root_name, ctors))
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
    pub fn relocate_all(
        &self,
        comp: &Compartment<Engine::Backing>,
        root_name: &str,
    ) -> Result<(), DynlinkError> {
        let root = self.lookup_loaded_library(comp, root_name)?;
        let rets = self.with_dfs_postorder(root, |item| match item {
            LoadedOrUnloaded::Unloaded(unlib) => {
                Err(DynlinkError::new(DynlinkErrorKind::LibraryLoadFail {
                    library: unlib.clone(),
                }))
            }
            LoadedOrUnloaded::Loaded(lib) => self.relocate_single(lib),
        });

        DynlinkError::collect(
            DynlinkErrorKind::RelocationFail {
                library: root_name.to_string(),
            },
            rets,
        )?;
        Ok(())
    }
}

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
