use dynlink::{
    context::Context,
    engines::{Backing, Engine},
};

use crate::init::InitDynlinkContext;

#[allow(dead_code)]
pub struct MonitorState {
    pub dynlink: *mut Context<Engine>,
    pub(crate) root: String,
}

impl MonitorState {
    pub(crate) fn new(init: InitDynlinkContext) -> Self {
        Self {
            dynlink: init.ctx,
            root: init.root,
        }
    }

    pub(crate) fn dynlink(&self) -> &Context<Engine> {
        unsafe { self.dynlink.as_ref().unwrap() }
    }

    pub(crate) fn get_nth_library(&self, n: usize) -> Option<&dynlink::library::Library<Backing>> {
        // TODO: this sucks.
        let mut all = vec![];
        let root = self.dynlink().lookup_library(&self.root)?;
        let root = match root {
            dynlink::context::LoadedOrUnloaded::Unloaded(_) => return None,
            dynlink::context::LoadedOrUnloaded::Loaded(lib) => lib,
        };
        self.dynlink()
            .with_bfs(root, |lib| all.push(lib.name().to_string()));
        all.get(n)
            .and_then(|x| match self.dynlink().lookup_library(&x) {
                Some(dynlink::context::LoadedOrUnloaded::Loaded(l)) => Some(l),
                _ => None,
            })
    }
}

unsafe impl Send for MonitorState {}
unsafe impl Sync for MonitorState {}
