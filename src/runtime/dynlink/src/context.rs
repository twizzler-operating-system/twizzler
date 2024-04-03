//! Management of global context.

use std::collections::HashMap;
use std::fmt::Display;

use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;

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

#[repr(C)]
/// A dynamic linker context, the main state struct for this crate.
pub struct Context<Engine: ContextEngine> {
    // Implementation callbacks.
    pub(crate) engine: Engine,
    // Track all the compartment names.
    compartment_names: HashMap<String, usize>,
    // Compartments get stable IDs from StableVec.
    compartments: Vec<Compartment<Engine::Backing>>,

    // This is the primary list of libraries, all libraries have an entry here, and they are
    // placed here independent of compartment. Edges denote dependency relationships, and may also cross compartments.
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
    /// Get the name of this library, loaded or unloaded.
    pub fn name(&self) -> &str {
        match self {
            LoadedOrUnloaded::Unloaded(unlib) => &unlib.name,
            LoadedOrUnloaded::Loaded(lib) => &lib.name,
        }
    }

    /// Get back a reference to the underlying loaded library, if loaded.
    pub fn loaded(&self) -> Option<&Library<Backing>> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
        }
    }

    /// Get back a mutable reference to the underlying loaded library, if loaded.
    pub fn loaded_mut(&mut self) -> Option<&mut Library<Backing>> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
        }
    }
}

impl<Engine: ContextEngine> Context<Engine> {
    /// Construct a new dynamic linker context.
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            compartment_names: HashMap::new(),
            library_deps: StableDiGraph::new(),
            compartments: Vec::new(),
        }
    }

    /// Lookup a compartment by name.
    pub fn lookup_compartment(&self, name: &str) -> Option<CompartmentId> {
        Some(CompartmentId(*self.compartment_names.get(name)?))
    }

    /// Get a reference to a compartment back by ID.
    pub fn get_compartment(
        &self,
        id: CompartmentId,
    ) -> Result<&Compartment<Engine::Backing>, DynlinkError> {
        if self.compartments.len() <= id.0 {
            return Err(DynlinkErrorKind::InvalidCompartmentId { id }.into());
        }
        Ok(&self.compartments[id.0])
    }

    /// Get a mut reference to a compartment back by ID.
    pub fn get_compartment_mut(
        &mut self,
        id: CompartmentId,
    ) -> Result<&mut Compartment<Engine::Backing>, DynlinkError> {
        if self.compartments.len() <= id.0 {
            return Err(DynlinkErrorKind::InvalidCompartmentId { id }.into());
        }
        Ok(&mut self.compartments[id.0])
    }

    /// Lookup a library by name
    pub fn lookup_library(&self, comp: CompartmentId, name: &str) -> Option<LibraryId> {
        let comp = self.get_compartment(comp).ok()?;
        Some(LibraryId(*comp.library_names.get(name)?))
    }

    /// Get a reference to a library back by ID.
    pub fn get_library(&self, id: LibraryId) -> Result<&Library<Engine::Backing>, DynlinkError> {
        if !self.library_deps.contains_node(id.0) {
            return Err(DynlinkErrorKind::InvalidLibraryId { id }.into());
        }
        match &self.library_deps[id.0] {
            LoadedOrUnloaded::Unloaded(unlib) => Err(DynlinkErrorKind::UnloadedLibrary {
                library: unlib.name.to_string(),
            }
            .into()),
            LoadedOrUnloaded::Loaded(lib) => Ok(lib),
        }
    }

    /// Get a mut reference to a library back by ID.
    pub fn get_library_mut(
        &mut self,
        id: LibraryId,
    ) -> Result<&mut Library<Engine::Backing>, DynlinkError> {
        if !self.library_deps.contains_node(id.0) {
            return Err(DynlinkErrorKind::InvalidLibraryId { id }.into());
        }
        match &mut self.library_deps[id.0] {
            LoadedOrUnloaded::Unloaded(unlib) => Err(DynlinkErrorKind::UnloadedLibrary {
                library: unlib.name.to_string(),
            }
            .into()),
            LoadedOrUnloaded::Loaded(lib) => Ok(lib),
        }
    }

    /// Traverse the library graph with DFS postorder, calling the callback for each library.
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

    /// Traverse the library graph with DFS postorder, calling the callback for each library (mutable ref).
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

    /// Traverse the library graph with BFS, calling the callback for each library.
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

    pub fn libraries(&self) -> LibraryIter<'_, Engine> {
        LibraryIter { ctx: self, next: 0 }
    }

    pub(crate) fn add_library(&mut self, lib: UnloadedLibrary) -> NodeIndex {
        self.library_deps.add_node(LoadedOrUnloaded::Unloaded(lib))
    }

    pub(crate) fn add_dep(&mut self, parent: NodeIndex, dep: NodeIndex) {
        self.library_deps.add_edge(parent, dep, ());
    }

    pub fn add_manual_dependency(&mut self, parent: LibraryId, dependee: LibraryId) {
        self.add_dep(parent.0, dependee.0);
    }

    /// Create a new compartment with a given name.
    pub fn add_compartment(&mut self, name: impl ToString) -> Result<CompartmentId, DynlinkError> {
        let name = name.to_string();
        let idx = self.compartments.len();
        let comp = Compartment::new(name.clone(), CompartmentId(idx));
        self.compartments.push(comp);
        self.compartment_names.insert(name, idx);
        Ok(CompartmentId(idx))
    }
}

pub struct LibraryIter<'a, Engine: ContextEngine> {
    ctx: &'a Context<Engine>,
    next: usize,
}

impl<'a, Engine: ContextEngine> Iterator for LibraryIter<'a, Engine> {
    type Item = &'a Library<Engine::Backing>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let idx = self.ctx.library_deps.node_indices().nth(self.next)?;
            self.next += 1;
            let node = &self.ctx.library_deps[idx];
            match node {
                LoadedOrUnloaded::Unloaded(_) => {}
                LoadedOrUnloaded::Loaded(lib) => return Some(lib),
            }
        }
    }
}
