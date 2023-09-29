use std::collections::BTreeMap;

use crate::{
    library::{internal::InternalLibrary, LibraryCollection, LibraryId},
    symbol::{Symbol, SymbolName},
    LookupError,
};

use super::CompartmentId;

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Clone)]
pub(crate) struct InternalCompartment {
    pub id: CompartmentId,
    pub libraries: BTreeMap<LibraryId, InternalLibrary>,
    pub name_map: BTreeMap<String, LibraryId>,
    pub dep_start: Option<LibraryId>,
    pub name: String,
}

impl InternalCompartment {
    pub(crate) fn insert_library(&mut self, lib: InternalLibrary) -> bool {
        if self.name_map.contains_key(lib.name()) {
            return false;
        }
        self.name_map.insert(lib.name().to_string(), lib.id());
        self.libraries.insert(lib.id(), lib);
        true
    }

    pub(crate) fn insert_all<L: Into<InternalLibrary>>(
        &mut self,
        coll: LibraryCollection<L>,
    ) -> bool {
        let mut first = true;
        for lib in coll {
            if !self.insert_library(lib.into()) && first {
                return false;
            }
            first = false;
        }
        true
    }

    pub(crate) fn lookup_symbol<Sym: Symbol + From<elf::symbol::Symbol>>(
        &self,
        name: &SymbolName,
    ) -> Result<Sym, LookupError> where {
        for lib in self.libraries.values() {
            if let Ok(sym) = lib.lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(LookupError::NotFound)
    }
}

impl InternalCompartment {
    pub fn new(name: String, id: CompartmentId, dep_start: Option<LibraryId>) -> Self {
        Self {
            libraries: Default::default(),
            id,
            dep_start,
            name_map: Default::default(),
            name,
        }
    }
}

impl core::fmt::Display for InternalCompartment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}
