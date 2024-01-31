use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use dynlink::{
    compartment::Compartment,
    context::Context,
    engines::{Backing, Engine},
};
use twizzler_object::ObjID;

use crate::{compartment::Comp, init::InitDynlinkContext};

pub struct MonitorState {
    pub dynlink: &'static mut Context<Engine>,
    pub(crate) root: String,

    pub comps: HashMap<ObjID, Comp>,
}

impl MonitorState {
    pub(crate) fn new(init: InitDynlinkContext) -> Self {
        Self {
            dynlink: unsafe { init.ctx.as_mut().unwrap() },
            root: init.root,
            comps: Default::default(),
        }
    }

    pub(crate) fn get_monitor_compartment_mut(&mut self) -> &mut Compartment<Backing> {
        let mid = self.dynlink.lookup_compartment("monitor").unwrap();
        self.dynlink.get_compartment_mut(mid).unwrap()
    }

    pub(crate) fn get_nth_library(&self, n: usize) -> Option<&dynlink::library::Library<Backing>> {
        // TODO: this sucks.
        let mut all = vec![];
        // TODO
        let comp_id = self.dynlink.lookup_compartment("monitor")?;
        let root_id = self.dynlink.lookup_library(comp_id, &self.root)?;
        self.dynlink
            .with_bfs(root_id, |lib| all.push(lib.name().to_string()));
        let lib = all
            .get(n)
            // TODO
            .and_then(|x| self.dynlink.lookup_library(comp_id, x))?;

        self.dynlink.get_library(lib).ok()
    }

    pub(crate) fn add_comp(&mut self, comp: Comp) {
        self.comps.insert(comp.sctx_id, comp);
    }
}

static MONITOR_STATE: OnceLock<Arc<Mutex<MonitorState>>> = OnceLock::new();

pub(crate) fn set_monitor_state(state: Arc<Mutex<MonitorState>>) {
    MONITOR_STATE
        .set(state)
        .unwrap_or_else(|_| panic!("monitor state already set"))
}

pub(crate) fn get_monitor_state() -> &'static Arc<Mutex<MonitorState>> {
    MONITOR_STATE
        .get()
        .unwrap_or_else(|| panic!("failed to get monitor state"))
}
