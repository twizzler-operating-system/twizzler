use std::sync::Mutex;

use dynlink::{
    context::Context,
    engines::{Backing, Engine},
};

use crate::init::InitDynlinkContext;

#[allow(dead_code)]
pub struct MonitorState {
    pub dynlink: &'static mut Context<Engine>,
    pub(crate) root: String,
}

impl MonitorState {
    pub(crate) fn new(init: InitDynlinkContext) -> Self {
        Self {
            dynlink: unsafe { init.ctx.as_mut().unwrap() },
            root: init.root,
        }
    }

    pub(crate) fn get_nth_library(&self, n: usize) -> Option<&dynlink::library::Library<Backing>> {
        // TODO: this sucks.
        let mut all = vec![];
        // TODO
        let comp = self.dynlink.get_compartment("monitor")?;
        let root = self.dynlink.lookup_library(comp, &self.root)?;
        let root = match root {
            dynlink::context::LoadedOrUnloaded::Unloaded(_) => return None,
            dynlink::context::LoadedOrUnloaded::Loaded(lib) => lib,
        };
        self.dynlink
            .with_bfs(root, |lib| all.push(lib.name().to_string()));
        let lib = all
            .get(n)
            // TODO
            .and_then(|x| match self.dynlink.lookup_library(comp, &x) {
                Some(dynlink::context::LoadedOrUnloaded::Loaded(l)) => Some(l),
                _ => None,
            });

        lib
    }
}

unsafe impl Send for MonitorState {}
unsafe impl Sync for MonitorState {}
