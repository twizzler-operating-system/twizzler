use crate::{
    context::Context,
    library::{Library, LibraryId, UnloadedLibrary},
    AddLibraryError, AdvanceError,
};

use super::{ReadyCompartment, UninitializedCompartment};

impl UninitializedCompartment {
    pub fn add_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
    ) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        let lib = lib.load(ctx)?;
        let lib = lib.relocate(ctx)?;
        self.int.insert_library(lib);
        Ok(id)
    }

    pub fn advance(self, _ctx: &mut Context) -> Result<ReadyCompartment, AdvanceError> {
        Ok(ReadyCompartment { int: todo!() })
    }
}
