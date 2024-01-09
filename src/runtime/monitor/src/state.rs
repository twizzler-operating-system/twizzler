use dynlink::{
    context::Context,
    engines::{Backing, Engine},
};

use crate::init::InitDynlinkContext;

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
        let comp_id = self.dynlink.lookup_compartment("monitor")?;
        let comp = self.dynlink.get_compartment(comp_id).ok()?;
        let root_id = self.dynlink.lookup_library(comp, &self.root)?;
        self.dynlink
            .with_bfs(root_id, |lib| all.push(lib.name().to_string()));
        let lib = all
            .get(n)
            // TODO
            .and_then(|x| match self.dynlink.lookup_library(comp, &x) {
                Some(l) => Some(l),
                _ => None,
            })?;

        self.dynlink.get_library(lib).ok()
    }
}
