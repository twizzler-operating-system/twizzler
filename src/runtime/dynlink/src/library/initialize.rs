use crate::{context::Context, AdvanceError};

use super::{ReadyLibrary, UninitializedLibrary};

impl UninitializedLibrary {
    pub fn initialize(self, _ctx: &mut Context) -> Result<ReadyLibrary, AdvanceError> {
        todo!()
    }
}
