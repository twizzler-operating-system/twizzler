//! Management of global context.

use std::collections::HashMap;
use std::fmt::Display;

use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use stable_vec::StableVec;

use crate::compartment::CompartmentId;
use crate::library::LibraryId;
use crate::DynlinkErrorKind;
use crate::{
    compartment::Compartment,
    library::{BackingData, Library, UnloadedLibrary},
    DynlinkError,
};

use self::engine::ContextEngine;

mod deps;
pub mod engine;
mod load;
mod relocate;
pub mod runtime;
mod syms;

pub struct Context<Engine: ContextEngine> {
    pub(crate) engine: Engine,
    // Track all the compartment names.
    compartment_names: HashMap<String, usize>,
    compartments: StableVec<Compartment<Engine::Backing>>,

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

    pub fn loaded(&self) -> Option<&Library<Backing>> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
        }
    }

    pub fn loaded_mut(&mut self) -> Option<&mut Library<Backing>> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
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
            compartments: StableVec::new(),
        }
    }

    pub fn lookup_compartment(&self, name: &str) -> Option<CompartmentId> {
        Some(CompartmentId(*self.compartment_names.get(name)?))
    }

    pub fn get_compartment(&self, id: CompartmentId) -> &Compartment<Engine::Backing> {
        &self.compartments[id.0]
    }

    pub fn get_compartment_mut(&mut self, id: CompartmentId) -> &mut Compartment<Engine::Backing> {
        &mut self.compartments[id.0]
    }

    /// Lookup a library by name
    pub fn lookup_library(
        &self,
        comp: &Compartment<Engine::Backing>,
        name: &str,
    ) -> Option<LibraryId> {
        Some(LibraryId(*comp.library_names.get(name)?))
    }

    pub fn get_library(&self, id: LibraryId) -> Result<&Library<Engine::Backing>, DynlinkError> {
        // TODO: can this panic?
        match &self.library_deps[id.0] {
            LoadedOrUnloaded::Unloaded(unlib) => Err(DynlinkErrorKind::UnloadedLibrary {
                library: unlib.name.to_string(),
            }
            .into()),
            LoadedOrUnloaded::Loaded(lib) => Ok(lib),
        }
    }

    pub fn get_library_mut(
        &mut self,
        id: LibraryId,
    ) -> Result<&mut Library<Engine::Backing>, DynlinkError> {
        // TODO: can this panic?
        match &mut self.library_deps[id.0] {
            LoadedOrUnloaded::Unloaded(unlib) => Err(DynlinkErrorKind::UnloadedLibrary {
                library: unlib.name.to_string(),
            }
            .into()),
            LoadedOrUnloaded::Loaded(lib) => Ok(lib),
        }
    }

    pub fn with_dfs_postorder<R>(
        &self,
        root_id: LibraryId,
        mut f: impl FnMut(&LoadedOrUnloaded<Engine::Backing>) -> R,
    ) -> Vec<R> {
        let mut rets = vec![];
        let mut visit = petgraph::visit::DfsPostOrder::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            rets.push(f(dep))
        }
        rets
    }

    pub fn with_dfs_postorder_mut<R>(
        &mut self,
        root_id: LibraryId,
        mut f: impl FnMut(&mut LoadedOrUnloaded<Engine::Backing>) -> R,
    ) -> Vec<R> {
        let mut rets = vec![];
        let mut visit = petgraph::visit::DfsPostOrder::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &mut self.library_deps[node];
            rets.push(f(dep))
        }
        rets
    }

    pub fn with_bfs(
        &self,
        root_id: LibraryId,
        mut f: impl FnMut(&LoadedOrUnloaded<Engine::Backing>),
    ) {
        let mut visit = petgraph::visit::Bfs::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            f(dep)
        }
    }

    pub(crate) fn add_library(&mut self, lib: UnloadedLibrary) -> NodeIndex {
        self.library_deps.add_node(LoadedOrUnloaded::Unloaded(lib))
    }

    pub(crate) fn add_dep(&mut self, parent: &Library<Engine::Backing>, dep: NodeIndex) {
        self.library_deps.add_edge(parent.idx, dep, ());
    }

    /// Create a new compartment with a given name.
    pub fn add_compartment(&mut self, name: impl ToString) -> Result<CompartmentId, DynlinkError> {
        let name = name.to_string();
        let comp = Compartment::new(name.clone());
        let idx = self.compartments.push(comp);
        self.compartment_names.insert(name, idx);
        Ok(CompartmentId(idx))
    }
}
