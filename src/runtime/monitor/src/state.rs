use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use dynlink::{
    compartment::Compartment,
    context::Context,
    engines::{Backing, Engine},
    library::BackingData,
};
use secgate::GateCallInfo;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_object::ObjID;
use twizzler_runtime_api::LibraryId;
use twz_rt::preinit_println;

use crate::{compartment::Comp, gates::LibraryInfo, init::InitDynlinkContext};

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

    pub(crate) fn add_comp(&mut self, mut comp: Comp, root_id: LibraryId) {
        comp.set_root_id(root_id);
        self.comps.insert(comp.sctx_id, comp);
    }

    pub(crate) fn lookup_comp(&self, sctx: ObjID) -> Option<&Comp> {
        self.comps.get(&sctx)
    }

    pub(crate) fn lookup_comp_mut(&mut self, sctx: ObjID) -> Option<&mut Comp> {
        self.comps.get_mut(&sctx)
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

pub(crate) fn try_get_monitor_state() -> Option<&'static Arc<Mutex<MonitorState>>> {
    MONITOR_STATE.get()
}

pub fn __monitor_rt_get_library_info(info: &GateCallInfo, id: LibraryId) -> Option<LibraryInfo> {
    let state = get_monitor_state().lock().unwrap();
    let lib = state.dynlink.get_library(id.into()).ok()?;
    let comp = state.lookup_comp(info.source_context().unwrap_or(0.into()))?;

    let compartment = state.dynlink.get_compartment(lib.compartment()).ok()?;
    if compartment.id != comp.compartment_id {
        //return None;
    }

    let handle = lib.full_obj.inner();

    let next_lib = state
        .dynlink
        .libraries()
        .skip_while(|l| lib.id() != l.id())
        .skip(1)
        .skip_while(|l| l.compartment() != lib.compartment())
        .next();

    Some(LibraryInfo {
        objid: handle.id,
        slot: handle.start as usize / MAX_SIZE,
        range: twizzler_runtime_api::AddrRange {
            start: handle.start as usize + NULLPAGE_SIZE,
            len: MAX_SIZE - NULLPAGE_SIZE,
        },
        dl_info: twizzler_runtime_api::DlPhdrInfo {
            addr: lib.base_addr(),
            name: core::ptr::null(),
            phdr_start: lib.get_phdrs_raw()?.0 as *const _,
            phdr_num: lib.get_phdrs_raw()?.1 as u32,
            _adds: 0,
            _subs: 0,
            modid: lib.tls_id.map(|t| t.tls_id()).unwrap_or(0) as usize,
            tls_data: core::ptr::null(),
        },
        next_id: next_lib.map(|nl| nl.id().into()),
    })
}
