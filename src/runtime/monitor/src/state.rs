use dynlink::{context::Context, library::LibraryRef};

use crate::init::InitDynlinkContext;

#[allow(dead_code)]
pub struct MonitorState {
    pub(crate) dynlink: Context,
    pub(crate) roots: Vec<LibraryRef>,
}

impl MonitorState {
    pub(crate) fn new(init: InitDynlinkContext) -> Self {
        Self {
            dynlink: init.ctx,
            roots: init.roots,
        }
    }
}
