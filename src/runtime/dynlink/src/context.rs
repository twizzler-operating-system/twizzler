use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use petgraph::stable_graph::StableDiGraph;
use tracing::debug;

use crate::{
    compartment::{Compartment, CompartmentRef},
    library::{Library, LibraryLoader, LibraryRef},
    symbol::RelocatedSymbol,
    DynlinkError,
};

#[derive(Default)]
pub(crate) struct ContextInner {
    id_counter: u128,
    id_stack: Vec<u128>,

    compartment_names: HashMap<String, CompartmentRef>,

    pub(crate) library_names: HashMap<String, LibraryRef>,
    library_deps: StableDiGraph<LibraryRef, ()>,
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

    pub fn lookup_symbol(
        &mut self,
        name: &str,
        primary: &CompartmentRef,
    ) -> Result<RelocatedSymbol, anyhow::Error> {
        if let Ok(sym) = primary.lookup_symbol(name) {
            return Ok(sym);
        }

        for comp in self.compartment_names.values() {
            if comp == primary {
                continue;
            }
            if let Ok(sym) = comp.lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(DynlinkError::NotFound {
            name: name.to_string(),
        }
        .into())
    }
}

#[derive(Default)]
pub struct Context {
    inner: Mutex<ContextInner>,
}

impl Context {
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

    pub fn add_library(
        &self,
        compartment: &CompartmentRef,
        lib: Library,
        loader: &mut impl LibraryLoader,
    ) -> Result<LibraryRef, DynlinkError> {
        let mut inner = self.inner.lock()?;

        compartment.load_library(lib, &mut inner, loader)
    }
}
