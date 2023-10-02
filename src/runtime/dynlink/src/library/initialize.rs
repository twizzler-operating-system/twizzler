use crate::{context::ContextInner, DynlinkError};

use super::Library;

impl Library {
    pub fn initialize(self, _ctx: &mut ContextInner) -> Result<(), DynlinkError> {
        todo!()
    }
}
