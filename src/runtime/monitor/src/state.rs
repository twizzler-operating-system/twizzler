use dynlink::{context::Context, library::LibraryRef};

use crate::init::InitDynlinkContext;

#[allow(dead_code)]
pub struct MonitorState {
    pub(crate) dynlink: Context,
    pub(crate) roots: Vec<LibraryRef>,
    pub(crate) library_list: Vec<LibraryRef>,
}

impl MonitorState {
    pub(crate) fn new(init: InitDynlinkContext) -> Self {
        let mut all = vec![];
        init.ctx
            .with_inner(|inner| {
                inner.with_bfs(&init.roots, |lib| {
                    all.push(lib.clone());
                })
            })
            .expect("failed to generate initial library list");

        Self {
            dynlink: init.ctx,
            roots: init.roots,
            library_list: all,
        }
    }

    pub(crate) fn get_nth_library(&self, n: usize) -> Option<LibraryRef> {
        self.library_list.get(n).cloned()
    }
}
