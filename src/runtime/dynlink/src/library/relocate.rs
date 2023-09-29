use tracing::debug;
use twizzler_object::Object;

use crate::{compartment::internal::InternalCompartment, AdvanceError};

use super::{
    internal::InternalLibrary, LibraryCollection, LibraryId, UnloadedLibrary, UnrelocatedLibrary,
};

impl UnrelocatedLibrary {
    pub(crate) fn new(
        old: UnloadedLibrary,
        data: Object<u8>,
        text: Object<u8>,
        deps: Vec<LibraryId>,
    ) -> Self {
        let mut next_int = old.int.clone();
        next_int.set_maps(data, text);
        next_int.set_deps(deps);
        Self { int: next_int }
    }
}

impl InternalLibrary {
    pub(crate) fn relocate(
        &self,
        _supplemental: Option<&LibraryCollection<UnrelocatedLibrary>>,
        _comp: &InternalCompartment,
    ) -> Result<(), AdvanceError> {
        debug!("relocating library {}", self);
        todo!()
    }
}
