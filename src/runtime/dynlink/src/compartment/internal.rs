use std::collections::{btree_map::IntoValues, BTreeMap};

use crate::{
    library::{Library, LibraryId},
    symbol::SymbolName,
    LookupError,
};

use super::CompartmentId;

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Clone)]
pub(super) struct InternalCompartment<L> {
    id: CompartmentId,
    libraries: BTreeMap<LibraryId, L>,
    dep_start: Option<LibraryId>,
}

impl<L: Library> InternalCompartment<L> {
    pub(super) fn id(&self) -> CompartmentId {
        self.id
    }

    pub(super) fn insert_library(&mut self, lib: L) {
        self.libraries.insert(lib.id(), lib);
    }

    pub(super) fn lookup_symbol(&self, name: &SymbolName) -> Result<L::SymbolType, LookupError> where
    {
        for lib in self.libraries.values() {
            if let Ok(sym) = lib.lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(LookupError::NotFound)
    }

    pub fn into_values(self) -> IntoValues<LibraryId, L> {
        self.libraries.into_values()
    }

    pub(super) fn dep_start(&self) -> Option<LibraryId> {
        self.dep_start
    }
}

impl<T> InternalCompartment<T> {
    pub fn new(id: CompartmentId, dep_start: Option<LibraryId>) -> Self {
        Self {
            libraries: Default::default(),
            id,
            dep_start,
        }
    }
}

impl<L: Library> core::fmt::Display for InternalCompartment<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Compartment_{}[{}]", self.id.0, L::state())
    }
}
