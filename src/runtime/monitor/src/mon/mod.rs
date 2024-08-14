use std::{cell::OnceCell, sync::OnceLock};

use dynlink::compartment::MONITOR_COMPARTMENT_ID;
use happylock::{LockCollection, RwLock, ThreadKey};
use monitor_api::SharedCompConfig;
use twizzler_runtime_api::{MapFlags, SpawnError};

use self::{
    compartment::{CompConfigObject, RunComp},
    space::Unmapper,
    thread::ManagedThread,
};
use crate::{api::MONITOR_INSTANCE_ID, init::InitDynlinkContext};

pub(crate) mod compartment;
pub(crate) mod space;
pub(crate) mod thread;

/// A security monitor instance. All monitor logic is implemented as methods for this type.
/// We split the state into the following components: 'space', managing the virtual memory space and
/// mapping objects, 'thread_mgr', which manages all threads owned by the monitor (typically, all
/// threads started by compartments), 'compartments', which manages compartment state, and
/// 'dynlink', which contains the dynamic linker state. The unmapper allows for background unmapping
/// and cleanup of objects and handles.
pub struct Monitor {
    locks: LockCollection<MonitorInner<'static>>,
    unmapper: OnceLock<Unmapper>,
    pub space: &'static RwLock<space::Space>,
    pub thread_mgr: &'static RwLock<thread::ThreadMgr>,
    pub comp_mgr: &'static RwLock<compartment::CompartmentMgr>,
    pub dynlink: &'static RwLock<&'static mut dynlink::context::Context>,
}

type MonitorInner<'a> = (
    &'a RwLock<space::Space>,
    &'a RwLock<thread::ThreadMgr>,
    &'a RwLock<compartment::CompartmentMgr>,
    &'a RwLock<&'static mut dynlink::context::Context>,
);

impl Monitor {
    pub fn start_background_threads(&mut self) {
        self.unmapper.set(Unmapper::new()).ok().unwrap();
        self.thread_mgr
            .write(ThreadKey::get().unwrap())
            .start_cleaner();
    }

    pub fn new(init: InitDynlinkContext) -> Self {
        let mut comp_mgr = compartment::CompartmentMgr::default();
        let mut space = space::Space::default();
        let monitor_scc = SharedCompConfig::new(MONITOR_INSTANCE_ID, std::ptr::null_mut());
        let handle = space
            .safe_create_and_map_runtime_object(
                MONITOR_INSTANCE_ID,
                MapFlags::READ | MapFlags::WRITE,
            )
            .unwrap();
        comp_mgr.insert(RunComp::new(
            MONITOR_INSTANCE_ID,
            MONITOR_INSTANCE_ID,
            "monitor".to_string(),
            MONITOR_COMPARTMENT_ID,
            vec![],
            CompConfigObject::new(handle, monitor_scc),
            0,
        ));

        let space = Box::leak(Box::new(RwLock::new(space)));
        let thread_mgr = Box::leak(Box::new(RwLock::new(thread::ThreadMgr::default())));
        let comp_mgr = Box::leak(Box::new(RwLock::new(comp_mgr)));
        let dynlink = Box::leak(Box::new(RwLock::new(unsafe { init.ctx.as_mut().unwrap() })));

        Self {
            locks: LockCollection::try_new((&*space, &*thread_mgr, &*comp_mgr, &*dynlink)).unwrap(),
            unmapper: OnceLock::new(),
            space,
            thread_mgr,
            comp_mgr,
            dynlink,
        }
    }

    pub fn start_thread(&self, main: Box<dyn FnOnce()>) -> Result<ManagedThread, SpawnError> {
        let key = ThreadKey::get().unwrap();
        let locks = &mut *self.locks.lock(key);

        let monitor_dynlink_comp = locks.3.get_compartment_mut(MONITOR_COMPARTMENT_ID).unwrap();
        locks
            .1
            .start_thread(&mut *locks.0, monitor_dynlink_comp, main)
    }
}

static MONITOR: OnceLock<Monitor> = OnceLock::new();

/// Get the monitor instance. Panics if called before first call to [set_monitor].
pub fn get_monitor() -> &'static Monitor {
    MONITOR.get().unwrap()
}

/// Set the monitor instance. Can only be called once. Must be called before any call to
/// [get_monitor].
pub fn set_monitor(monitor: Monitor) {
    if MONITOR.set(monitor).is_err() {
        panic!("second call to set_monitor");
    }
}

pub use space::early_object_map;
