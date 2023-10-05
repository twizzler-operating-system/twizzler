use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use petgraph::stable_graph::StableDiGraph;
use tracing::{debug, trace};

use crate::{
    compartment::{Compartment, CompartmentRef},
    library::{Library, LibraryLoader, LibraryRef},
    symbol::RelocatedSymbol,
    DynlinkError, ECollector,
};

struct NameContext<'a> {
    pub(crate) lib: &'a Library,
}

impl<'a> From<&'a Library> for NameContext<'a> {
    fn from(lib: &'a Library) -> Self {
        Self { lib }
    }
}

#[derive(Default)]
pub(crate) struct ContextInner {
    id_counter: u128,
    id_stack: Vec<u128>,

    compartment_names: HashMap<String, CompartmentRef>,
    pub(crate) compartment_map: HashMap<u128, CompartmentRef>,

    pub(crate) library_names: HashMap<String, LibraryRef>,
    pub(crate) library_deps: StableDiGraph<LibraryRef, ()>,
}

impl ContextInner {
    fn get_fresh_id(&mut self) -> u128 {
        if let Some(old) = self.id_stack.pop() {
            old
        } else {
            self.id_counter += 1;
            self.id_counter
        }
    }

    pub(crate) fn with_ctors<I>(&self, roots: I, mut f: impl FnMut(&LibraryRef))
    where
        I: IntoIterator<Item = LibraryRef>,
    {
        for root in roots.into_iter() {
            let mut visit =
                petgraph::visit::DfsPostOrder::new(&self.library_deps, root.idx.get().unwrap());
            while let Some(node) = visit.next(&self.library_deps) {
                let dep = &self.library_deps[node];
                f(dep)
            }
        }
    }

    pub(crate) fn insert_lib(
        &mut self,
        lib: LibraryRef,
        deps: impl IntoIterator<Item = LibraryRef>,
    ) {
        self.library_names.insert(lib.name.clone(), lib.clone());
        lib.idx.set(Some(self.library_deps.add_node(lib.clone())));
        for dep in deps.into_iter() {
            self.library_deps
                .add_edge(lib.idx.get().unwrap(), dep.idx.get().unwrap(), ());
        }
    }

    pub(crate) fn lookup_symbol(
        &self,
        ctx: &LibraryRef,
        name: &str,
    ) -> Result<RelocatedSymbol, DynlinkError> {
        if let Ok(sym) = ctx.lookup_symbol(name) {
            return Ok(sym);
        }

        if let Some(sym) = self
            .library_deps
            .neighbors_directed(ctx.idx.get().unwrap(), petgraph::Direction::Outgoing)
            .find_map(|depidx| {
                let dep = &self.library_deps[depidx];
                if depidx != ctx.idx.get().unwrap() {
                    self.lookup_symbol(dep, name).ok()
                } else {
                    None
                }
            })
        {
            return Ok(sym);
        }
        trace!("falling back to global search for {}", name);
        self.lookup_symbol_global(name)
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
}

#[derive(Default)]
pub struct Context {
    inner: Mutex<ContextInner>,
}

impl Context {
    pub(crate) fn with_inner_mut<R>(
        &self,
        f: impl FnOnce(&mut ContextInner) -> R,
    ) -> Result<R, DynlinkError> {
        Ok(f(&mut *self.inner.lock()?))
    }

    pub(crate) fn with_inner<R>(
        &self,
        f: impl FnOnce(&ContextInner) -> R,
    ) -> Result<R, DynlinkError> {
        Ok(f(&*self.inner.lock()?))
    }

    pub fn add_compartment(&self, name: impl ToString) -> Result<CompartmentRef, DynlinkError> {
        let name = name.to_string();
        let mut inner = self.inner.lock()?;
        if inner.compartment_names.contains_key(&name) {
            return Err(DynlinkError::AlreadyExists { name });
        }
        let id = inner.get_fresh_id();
        let compartment = Arc::new(Compartment::new(name.clone(), id));
        inner.compartment_names.insert(name, compartment.clone());

        Ok(compartment)
    }

    pub fn lookup_symbol(
        &self,
        ctx: &LibraryRef,
        name: &str,
    ) -> Result<RelocatedSymbol, DynlinkError> {
        self.inner.lock()?.lookup_symbol(ctx, name)
    }

    pub fn add_library(
        &self,
        compartment: &CompartmentRef,
        lib: Library,
        loader: &mut impl LibraryLoader,
    ) -> Result<LibraryRef, DynlinkError> {
        let mut inner = self.inner.lock()?;

        compartment.load_library(lib, &mut inner, loader)
    }

    pub fn relocate_all(
        &self,
        roots: impl IntoIterator<Item = LibraryRef>,
        loader: &mut impl LibraryLoader,
    ) -> Result<Vec<LibraryRef>, DynlinkError> {
        let inner = self.inner.lock()?;

        roots
            .into_iter()
            .map(|root| {
                debug!("relocate_all: relocation root: {}", root);
                root.relocate(&*inner).map(|_| root)
            })
            .ecollect()
    }
}
