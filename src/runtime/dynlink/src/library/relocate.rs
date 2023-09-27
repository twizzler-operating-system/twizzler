use crate::{context::Context, AdvanceError};

use super::{UninitializedLibrary, UnrelocatedLibrary};

impl UnrelocatedLibrary {
    pub fn relocate(self, _ctx: &mut Context) -> Result<UninitializedLibrary, AdvanceError> {
        todo!()
    }
}
