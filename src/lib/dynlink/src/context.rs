//! Management of global context.

use std::{collections::HashMap, fmt::Display};

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use stable_vec::StableVec;

use crate::{
    compartment::{Compartment, CompartmentId},
    engines::ContextEngine,
    library::{Library, LibraryId, UnloadedLibrary},
    DynlinkError, DynlinkErrorKind,
};

mod deps;
mod load;
pub(crate) mod relocate;
pub mod runtime;
mod syms;

pub use load::LoadIds;

#[repr(C)]
/// A dynamic linker context, the main state struct for this crate.
pub struct Context {
    // Implementation callbacks.
    pub(crate) engine: Box<dyn ContextEngine + Send>,
    // Track all the compartment names.
    compartment_names: HashMap<String, usize>,
    // Compartments get stable IDs from StableVec.
    compartments: StableVec<Compartment>,

    // This is the primary list of libraries, all libraries have an entry here, and they are
    // placed here independent of compartment. Edges denote dependency relationships, and may also
    // cross compartments.
    pub(crate) library_deps: StableDiGraph<LoadedOrUnloaded, ()>,
}

// Libraries in the dependency graph are placed there before loading, so that they can participate
// in dependency search. So we need to track both kinds of libraries that may be at a given index in
// the graph.
pub enum LoadedOrUnloaded {
    Unloaded(UnloadedLibrary),
    Loaded(Library),
}

impl Display for LoadedOrUnloaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadedOrUnloaded::Unloaded(unlib) => write!(f, "(unloaded){}", unlib),
            LoadedOrUnloaded::Loaded(lib) => write!(f, "(loaded){}", lib),
        }
    }
}

impl LoadedOrUnloaded {
    /// Get the name of this library, loaded or unloaded.
    pub fn name(&self) -> &str {
        match self {
            LoadedOrUnloaded::Unloaded(unlib) => &unlib.name,
            LoadedOrUnloaded::Loaded(lib) => &lib.name,
        }
    }

    /// Get back a reference to the underlying loaded library, if loaded.
    pub fn loaded(&self) -> Option<&Library> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
        }
    }

    /// Get back a mutable reference to the underlying loaded library, if loaded.
    pub fn loaded_mut(&mut self) -> Option<&mut Library> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
        }
    }
}

impl Context {
    /// Construct a new dynamic linker context.
    pub fn new(engine: Box<dyn ContextEngine + Send>) -> Self {
        Self {
            engine,
            compartment_names: HashMap::new(),
            library_deps: StableDiGraph::new(),
            compartments: StableVec::new(),
        }
    }

    /// Replace the callback engine for this context.
    pub fn replace_engine(&mut self, engine: Box<dyn ContextEngine + Send>) {
        self.engine = engine;
    }

    /// Lookup a compartment by name.
    pub fn lookup_compartment(&self, name: &str) -> Option<CompartmentId> {
        Some(CompartmentId(*self.compartment_names.get(name)?))
    }

    /// Get a reference to a compartment back by ID.
    pub fn get_compartment(&self, id: CompartmentId) -> Result<&Compartment, DynlinkError> {
        if !self.compartments.has_element_at(id.0) {
            return Err(DynlinkErrorKind::InvalidCompartmentId { id }.into());
        }
        Ok(&self.compartments[id.0])
    }

    /// Get a mut reference to a compartment back by ID.
    pub fn get_compartment_mut(
        &mut self,
        id: CompartmentId,
    ) -> Result<&mut Compartment, DynlinkError> {
        if !self.compartments.has_element_at(id.0) {
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
    pub fn get_library(&self, id: LibraryId) -> Result<&Library, DynlinkError> {
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
    pub fn get_library_mut(&mut self, id: LibraryId) -> Result<&mut Library, DynlinkError> {
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
        mut f: impl FnMut(&LoadedOrUnloaded) -> R,
    ) -> Vec<R> {
        let mut rets = vec![];
        let mut visit = petgraph::visit::DfsPostOrder::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            rets.push(f(dep))
        }
        rets
    }

    /// Traverse the library graph with DFS postorder, calling the callback for each library
    /// (mutable ref).
    pub fn with_dfs_postorder_mut<R>(
        &mut self,
        root_id: LibraryId,
        mut f: impl FnMut(&mut LoadedOrUnloaded) -> R,
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
    pub fn with_bfs(&self, root_id: LibraryId, mut f: impl FnMut(&LoadedOrUnloaded) -> bool) {
        let mut visit = petgraph::visit::Bfs::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            if !f(dep) {
                return;
            }
        }
    }

    pub fn libraries(&self) -> LibraryIter<'_> {
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

    pub fn unload_compartment(
        &mut self,
        comp_id: CompartmentId,
    ) -> (Option<Compartment>, Vec<LoadedOrUnloaded>) {
        let Ok(comp) = self.get_compartment(comp_id) else {
            return (None, vec![]);
        };
        let name = comp.name.clone();
        let ids = comp.library_ids();
        let nodes = ids
            .collect::<Vec<_>>()
            .iter()
            .filter_map(|id| self.library_deps.remove_node(id.0))
            .collect();
        self.compartment_names.remove(&name);
        (self.compartments.remove(comp_id.0), nodes)
    }

    /// Create a new compartment with a given name.
    pub fn add_compartment(
        &mut self,
        name: impl ToString,
        new_comp_flags: NewCompartmentFlags,
    ) -> Result<CompartmentId, DynlinkError> {
        let name = name.to_string();
        let idx = self.compartments.next_push_index();
        let comp = Compartment::new(name.clone(), CompartmentId(idx), new_comp_flags);
        self.compartments.push(comp);
        tracing::debug!("added compartment {} with ID {}", name, idx);
        self.compartment_names.insert(name, idx);
        Ok(CompartmentId(idx))
    }

    /// Get a list of external compartments that the given compartment depends on.
    pub fn compartment_dependencies(
        &self,
        id: CompartmentId,
    ) -> Result<Vec<CompartmentId>, DynlinkError> {
        let comp = self.get_compartment(id)?;
        let mut deps = vec![];
        for lib in comp.library_ids() {
            for n in self.library_deps.neighbors(lib.0) {
                let neigh = self.library_deps[n].loaded().unwrap();
                deps.push(neigh.comp_id);
            }
        }
        deps.sort_unstable();
        deps.dedup();
        if let Some(dep) = deps.iter().position(|dep| *dep == id) {
            deps.remove(dep);
        }
        Ok(deps)
    }
}

pub struct LibraryIter<'a> {
    ctx: &'a Context,
    next: usize,
}

impl<'a> Iterator for LibraryIter<'a> {
    type Item = &'a Library;

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

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct NewCompartmentFlags : u32 {
        const EXPORT_GATES = 0x1;
        const DEBUG = 0x2;
    }
}
