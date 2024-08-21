use std::{ptr::NonNull, sync::OnceLock};

use dynlink::compartment::MONITOR_COMPARTMENT_ID;
use happylock::{LockCollection, RwLock, ThreadKey};
use monitor_api::{SharedCompConfig, TlsTemplateInfo};
use secgate::util::HandleMgr;
use twizzler_abi::upcall::UpcallFrame;
use twizzler_runtime_api::{LibraryId, MapError, MapFlags, ObjID, SpawnError, ThreadSpawnArgs};
use twz_rt::{RuntimeState, RuntimeThreadControl, OUR_RUNTIME};

use self::{
    compartment::{CompConfigObject, RunComp},
    space::{MapHandle, MapInfo, Unmapper},
    thread::{ManagedThread, ThreadCleaner},
};
use crate::{
    api::MONITOR_INSTANCE_ID,
    gates::{CompartmentInfo, LibraryInfo, LoadCompartmentError, LoadLibraryError},
    init::InitDynlinkContext,
};

pub(crate) mod compartment;
pub mod library;
pub(crate) mod space;
pub(crate) mod thread;

/// A security monitor instance. All monitor logic is implemented as methods for this type.
/// We split the state into the following components: 'space', managing the virtual memory space and
/// mapping objects, 'thread_mgr', which manages all threads owned by the monitor (typically, all
/// threads started by compartments), 'compartments', which manages compartment state, and
/// 'dynlink', which contains the dynamic linker state. The unmapper allows for background unmapping
/// and cleanup of objects and handles.
pub struct Monitor {
    locks: LockCollection<MonitorLocks<'static>>,
    unmapper: OnceLock<Unmapper>,
    /// Management of address space.
    pub space: &'static RwLock<space::Space>,
    /// Management of all threads.
    pub thread_mgr: &'static RwLock<thread::ThreadMgr>,
    /// Management of compartments.
    pub comp_mgr: &'static RwLock<compartment::CompartmentMgr>,
    /// Dynamic linker state.
    pub dynlink: &'static RwLock<&'static mut dynlink::context::Context>,
    /// Open handles to libraries.
    pub library_handles: &'static RwLock<HandleMgr<()>>,
}

// We allow locking individually, using eg mon.space.write(key), or locking the collection for more
// complex operations that touch multiple pieces of state.
type MonitorLocks<'a> = (
    &'a RwLock<space::Space>,
    &'a RwLock<thread::ThreadMgr>,
    &'a RwLock<compartment::CompartmentMgr>,
    &'a RwLock<&'static mut dynlink::context::Context>,
    &'a RwLock<HandleMgr<()>>,
);

impl Monitor {
    /// Start the background threads for the monitor instance. Must be done only once the monitor
    /// has been initialized.
    pub fn start_background_threads(&self) {
        assert!(OUR_RUNTIME.state().contains(RuntimeState::READY));
        let cleaner = ThreadCleaner::new();
        self.unmapper.set(Unmapper::new()).ok().unwrap();
        self.thread_mgr
            .write(ThreadKey::get().unwrap())
            .set_cleaner(cleaner);
    }

    /// Build a new monitor state from the initial dynamic linker context.
    pub fn new(init: InitDynlinkContext) -> Self {
        let mut comp_mgr = compartment::CompartmentMgr::default();
        let mut space = space::Space::default();

        // Build our TLS region, and create a template for the monitor compartment.
        let super_tls = (unsafe { &mut *init.ctx })
            .get_compartment_mut(MONITOR_COMPARTMENT_ID)
            .unwrap()
            .build_tls_region(RuntimeThreadControl::default(), |layout| unsafe {
                NonNull::new(std::alloc::alloc_zeroed(layout))
            })
            .unwrap();
        let template: &'static TlsTemplateInfo = Box::leak(Box::new(super_tls.into()));

        // Set up the monitor's compartment.
        let monitor_scc =
            SharedCompConfig::new(MONITOR_INSTANCE_ID, template as *const _ as *mut _);
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

        // Allocate and leak all the locks (they are global and eternal, so we can do this to safely
        // and correctly get &'static lifetime)
        let space = Box::leak(Box::new(RwLock::new(space)));
        let thread_mgr = Box::leak(Box::new(RwLock::new(thread::ThreadMgr::default())));
        let comp_mgr = Box::leak(Box::new(RwLock::new(comp_mgr)));
        let dynlink = Box::leak(Box::new(RwLock::new(unsafe { init.ctx.as_mut().unwrap() })));
        let library_handles = Box::leak(Box::new(RwLock::new(HandleMgr::new(None))));

        // Okay to call try_new here, since it's not many locks and only happens once.
        Self {
            locks: LockCollection::try_new((
                &*space,
                &*thread_mgr,
                &*comp_mgr,
                &*dynlink,
                &*library_handles,
            ))
            .unwrap(),
            unmapper: OnceLock::new(),
            space,
            thread_mgr,
            comp_mgr,
            dynlink,
            library_handles,
        }
    }

    /// Start a managed monitor thread.
    pub fn start_thread(&self, main: Box<dyn FnOnce()>) -> Result<ManagedThread, SpawnError> {
        let key = ThreadKey::get().unwrap();
        let locks = &mut *self.locks.lock(key);

        let monitor_dynlink_comp = locks.3.get_compartment_mut(MONITOR_COMPARTMENT_ID).unwrap();
        locks
            .1
            .start_thread(&mut *locks.0, monitor_dynlink_comp, main)
    }

    /// Spawn a thread into a given compartment, using initial thread arguments.
    pub fn spawn_compartment_thread(
        &self,
        instance: ObjID,
        args: ThreadSpawnArgs,
        stack_ptr: usize,
        thread_ptr: usize,
    ) -> Result<ObjID, SpawnError> {
        let thread = self.start_thread(Box::new(move || {
            let frame = UpcallFrame::new_entry_frame(
                stack_ptr,
                args.stack_size,
                thread_ptr,
                instance,
                args.start,
                args.arg,
            );
            unsafe { twizzler_abi::syscall::sys_thread_resume_from_upcall(&frame) };
        }))?;
        Ok(thread.id)
    }

    /// Get the compartment config for the given compartment.
    pub fn get_comp_config(&self, sctx: ObjID) -> Option<*const SharedCompConfig> {
        let comps = self.comp_mgr.read(ThreadKey::get().unwrap());
        Some(comps.get(sctx)?.comp_config_ptr())
    }

    /// Map an object into a given compartment.
    pub fn map_object(&self, sctx: ObjID, info: MapInfo) -> Result<MapHandle, MapError> {
        let handle = self.space.write(ThreadKey::get().unwrap()).map(info)?;

        let mut comp_mgr = self.comp_mgr.write(ThreadKey::get().unwrap());
        let rc = comp_mgr.get_mut(sctx).ok_or(MapError::InvalidArgument)?;
        let handle = rc.map_object(info, handle)?;
        Ok(handle)
    }

    pub fn get_thread_simple_buffer(&self, sctx: ObjID, thread: ObjID) -> Option<ObjID> {
        todo!()
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
