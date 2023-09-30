use std::collections::BTreeMap;

use talc::{ErrOnOom, Talc};
use twizzler_object::Object;

use crate::{
    library::{internal::InternalLibrary, LibraryCollection, LibraryId},
    symbol::{RelocatedSymbol, Symbol},
    LookupError,
};

use super::CompartmentId;

pub(crate) struct InternalCompartment {
    pub id: CompartmentId,
    pub libraries: BTreeMap<LibraryId, InternalLibrary>,
    pub name_map: BTreeMap<String, LibraryId>,
    pub dep_start: Option<LibraryId>,
    pub alloc_objects: Vec<Object<u8>>,
    pub allocator: Talc<ErrOnOom>,
    pub name: String,
}

impl InternalCompartment {
    pub(crate) fn new(
        id: CompartmentId,
        libraries: BTreeMap<LibraryId, InternalLibrary>,
        name_map: BTreeMap<String, LibraryId>,
        dep_start: Option<LibraryId>,
        alloc_objects: Vec<Object<u8>>,
        allocator: Talc<ErrOnOom>,
        name: String,
    ) -> Self {
        Self {
            id,
            libraries,
            name_map,
            dep_start,
            alloc_objects,
            allocator,
            name,
        }
    }

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

    pub(crate) fn lookup_symbol(&self, name: &str) -> Result<RelocatedSymbol, LookupError> where {
        for lib in self.libraries.values() {
            if let Ok(sym) = lib.lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(LookupError::NotFound)
    }
}

impl core::fmt::Display for InternalCompartment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}
