use tracing::debug;

use crate::{
    context::Context,
    library::{
        LibraryCollection, LibraryId, LibraryLoader, ReadyLibrary, SymbolResolver,
        UninitializedLibrary, UnloadedLibrary,
    },
    AddLibraryError, AdvanceError,
};

use super::{
    internal::InternalCompartment, LibraryResolver, UninitializedCompartment,
    UnrelocatedCompartment,
};

impl UninitializedCompartment {
    pub fn new(old: UnrelocatedCompartment, _ctx: &mut Context) -> Result<Self, AdvanceError> {
        debug!("relocating compartment {}", old.int);

        for lib in old.int.libraries.values() {
            lib.relocate(None, &old.int)?;
        }

        Ok(Self { int: old.int })
    }

    pub fn add_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
        resolver: &mut LibraryResolver,
        loader: &mut LibraryLoader,
    ) -> Result<LibraryId, AddLibraryError> {
        let id = lib.internal().id();
        let coll = self.int.load_library(lib, ctx, resolver, loader)?;
        let coll = self.int.relocate_collection(coll)?;
        if !self.int.insert_all(coll) {
            return Err(AddLibraryError::AdvanceError(AdvanceError::LibraryFailed(
                id,
            )));
        }
        Ok(id)
    }
}

impl InternalCompartment {
    pub(crate) fn initialize_collection(
        &mut self,
        _coll: LibraryCollection<UninitializedLibrary>,
    ) -> Result<LibraryCollection<ReadyLibrary>, AdvanceError> {
        todo!()
    }
}
