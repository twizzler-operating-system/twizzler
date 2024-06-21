use std::{
    alloc::Layout,
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use dynlink::{
    compartment::Compartment,
    context::{engine::ContextEngine, Context},
    engines::Engine,
};
use monitor_api::{SharedCompConfig, TlsTemplateInfo};
use twizzler_runtime_api::{MapError, MapFlags, ObjID};
use twz_rt::{preinit_println, RuntimeThreadControl};

use self::runcomp::{RunComp, RunCompInner};
use crate::{
    api::MONITOR_INSTANCE_ID,
    init::InitDynlinkContext,
    mapman::{MapHandle, MapInfo},
};

mod loader;
mod object;
mod runcomp;
mod stack_object;
mod thread;

pub(crate) struct CompMan {
    inner: Mutex<CompManInner>,
}

lazy_static::lazy_static! {
pub(crate) static ref COMPMAN: CompMan = CompMan::new();
pub(crate) static ref MONITOR_COMP: OnceLock<RunComp> = OnceLock::new();
}

impl CompMan {
    fn new() -> Self {
        Self {
            inner: Mutex::new(CompManInner::default()),
        }
    }
}

#[derive(Default)]
pub(crate) struct CompManInner {
    name_map: HashMap<String, ObjID>,
    instance_map: HashMap<ObjID, RunComp>,
    dynlink_state: Option<&'static mut Context<Engine>>,
}

impl CompManInner {
    pub fn dynlink(&self) -> &Context<Engine> {
        self.dynlink_state.as_ref().unwrap()
    }

    pub fn dynlink_mut(&mut self) -> &mut Context<Engine> {
        self.dynlink_state.as_mut().unwrap()
    }

    pub fn get_monitor_dynlink_compartment(
        &mut self,
    ) -> &mut Compartment<<Engine as ContextEngine>::Backing> {
        let id = MONITOR_COMP.get().unwrap().compartment_id;
        self.dynlink_mut().get_compartment_mut(id).unwrap()
    }

    pub fn insert(&mut self, rc: RunComp) {
        self.name_map.insert(rc.name().to_string(), rc.instance);
        self.instance_map.insert(rc.instance, rc);
    }

    pub fn lookup(&self, instance: ObjID) -> Option<&RunComp> {
        self.instance_map.get(&instance)
    }

    pub fn lookup_name(&mut self, name: &str) -> Option<&RunComp> {
        self.lookup(*self.name_map.get(name)?)
    }

    pub fn lookup_instance(&mut self, name: &str) -> Option<ObjID> {
        self.name_map.get(name).cloned()
    }

    pub fn remove(&mut self, instance: ObjID) -> Option<RunComp> {
        let Some(rc) = self.instance_map.remove(&instance) else {
            return None;
        };
        self.name_map.remove(rc.name());
        Some(rc)
    }
}

impl CompMan {
    pub fn init(&self, mut idc: InitDynlinkContext) {
        let mut cm = self.inner.lock().unwrap();
        cm.dynlink_state = Some(idc.ctx());

        let monitor_comp_id = cm.dynlink().lookup_compartment("monitor").unwrap();
        let monitor_root_id = cm
            .dynlink()
            .lookup_library(monitor_comp_id, &idc.root)
            .unwrap();
        let mon_rc = RunComp::new(
            MONITOR_INSTANCE_ID,
            MONITOR_INSTANCE_ID,
            "monitor",
            monitor_comp_id,
            monitor_root_id,
        )
        .expect("failed to bootstrap monitor RunComp");

        mon_rc.with_inner(|inner| {
            let tls = cm
                .dynlink_mut()
                .get_compartment_mut(monitor_comp_id)
                .unwrap()
                .build_tls_region(RuntimeThreadControl::new(0), |layout| unsafe {
                    inner.allocator.malloc(layout).ok()
                })
                .unwrap();
            let info = TlsTemplateInfo::from(tls);
            let template = unsafe {
                inner
                    .allocator
                    .malloc(Layout::new::<TlsTemplateInfo>())
                    .unwrap()
                    .as_ptr() as *mut TlsTemplateInfo
            };
            unsafe {
                template.write(info);
            }
            let config = SharedCompConfig::new(MONITOR_INSTANCE_ID, template);
            inner.comp_config_object().write_config(config);
        });

        MONITOR_COMP.set(mon_rc).unwrap();
    }

    pub fn lock(&self) -> MutexGuard<'_, CompManInner> {
        self.inner.lock().unwrap()
    }

    pub fn with_monitor_compartment<R>(&self, f: impl FnOnce(&RunComp) -> R) -> R {
        f(MONITOR_COMP.get().unwrap())
    }

    pub fn get_comp_inner(&self, comp_id: ObjID) -> Option<Arc<Mutex<RunCompInner>>> {
        if comp_id == MONITOR_INSTANCE_ID {
            return Some(MONITOR_COMP.get().unwrap().cloned_inner());
        }
        // Lock, get inner and clone, and release lock. Consumers of this function can then safely
        // lock the inner RC without holding the CompMan lock.
        let inner = self.inner.lock().ok()?;
        let rc = inner.lookup(comp_id)?;
        Some(rc.cloned_inner())
    }

    //it's this. But it's more than that -- we need to set up TLS for the monitor by the time we
    // get here, so we need to figure out how to do compartment entry properly.
    #[tracing::instrument(skip(self))]
    pub fn map_object(
        &self,
        comp_id: ObjID,
        id: ObjID,
        flags: MapFlags,
    ) -> Result<MapHandle, MapError> {
        preinit_println!("MO HE");
        tracing::warn!("==> mo");
        if comp_id == MONITOR_INSTANCE_ID {
            return MONITOR_COMP
                .get()
                .unwrap()
                .with_inner(|inner| inner.map_object(MapInfo { id, flags }));
        }
        let rc = self
            .get_comp_inner(comp_id)
            .ok_or(MapError::InternalError)?;
        let mut rc = rc.lock().map_err(|_| MapError::InternalError)?;
        rc.map_object(MapInfo { id, flags })
    }

    pub fn unmap_object(&self, comp_id: ObjID, id: ObjID, flags: MapFlags) -> Result<(), MapError> {
        if comp_id == MONITOR_INSTANCE_ID {
            MONITOR_COMP
                .get()
                .unwrap()
                .with_inner(|inner| inner.unmap_object(MapInfo { id, flags }));
            return Ok(());
        }
        let rc = self
            .get_comp_inner(comp_id)
            .ok_or(MapError::InternalError)?;
        let mut rc = rc.lock().map_err(|_| MapError::InternalError)?;
        rc.unmap_object(MapInfo { id, flags });
        Ok(())
    }
}
