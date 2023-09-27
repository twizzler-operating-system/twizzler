use twizzler_object::{ObjID, Object, ObjectInitError, ObjectInitFlags, Protections};

use crate::{compartment::CompartmentId, context::Context, AdvanceError};

use super::{internal::InternalLibrary, UnloadedLibrary, UnrelocatedLibrary, LibraryId};

impl UnloadedLibrary {
    pub fn load(&self, _cxt: &mut Context) -> Result<UnrelocatedLibrary, AdvanceError> {
        todo!()
    }

    pub fn new(
        id: ObjID,
        comp_id: CompartmentId,
        name: impl ToString,
    ) -> Result<Self, ObjectInitError> {
        let obj = Object::init_id(id, Protections::READ, ObjectInitFlags::empty())?;
        Ok(Self {
            int: InternalLibrary::new(obj, comp_id, Some(name.to_string()), LibraryId(id)),
        })
    }
}

impl core::fmt::Display for UnloadedLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        core::fmt::Display::fmt(&self.int, f)
    }
}
