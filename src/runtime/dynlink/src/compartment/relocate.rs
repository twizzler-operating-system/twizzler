use tracing::debug;

use crate::{
    context::Context,
    library::{
        LibraryCollection, LibraryId, LibraryLoader, UninitializedLibrary, UnloadedLibrary,
        UnrelocatedLibrary,
    },
    AddLibraryError, AdvanceError,
};

use super::{
    internal::InternalCompartment, LibraryResolver, UnloadedCompartment, UnrelocatedCompartment,
};

impl UnrelocatedCompartment {
    pub fn new(
        old: UnloadedCompartment,
        ctx: &mut Context,
        resolver: &mut LibraryResolver,
        loader: &mut LibraryLoader,
    ) -> Result<Self, AdvanceError> {
        debug!("loading compartment {}", old.int);
        let InternalCompartment {
            id,
            libraries,
            name_map: _name_map,
            dep_start,
            name,
        } = old.int;
        let mut next = InternalCompartment::new(name, id, dep_start);

        for lib in libraries.into_values() {
            let coll = next.load_library(UnloadedLibrary::from(lib), ctx, resolver, loader)?;
            next.insert_all(coll);
        }

        Ok(Self { int: next })
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
        if !self.int.insert_all(coll) {
            return Err(AddLibraryError::AdvanceError(AdvanceError::LibraryFailed(
                id,
            )));
        }
        Ok(id)
    }
}

impl InternalCompartment {
    pub(crate) fn relocate_collection(
        &mut self,
        _coll: LibraryCollection<UnrelocatedLibrary>,
    ) -> Result<LibraryCollection<UninitializedLibrary>, AdvanceError> {
        todo!()
    }
}
