use crate::{
    context::Context,
    library::{Library, LibraryId, UnloadedLibrary},
    AddLibraryError, AdvanceError,
};

use super::{UninitializedCompartment, UnrelocatedCompartment};

impl UnrelocatedCompartment {
    pub fn advance(self, _ctx: &mut Context) -> Result<UninitializedCompartment, AdvanceError> {
        Ok(UninitializedCompartment { int: todo!() })
    }

    pub fn add_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
    ) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        let lib = lib.load(ctx)?;
        self.int.insert_library(lib);
        Ok(id)
    }
}
