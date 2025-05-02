use std::{
    ptr::NonNull,
    sync::{Mutex, OnceLock},
};

use compartment::{
    StackObject, COMP_DESTRUCTED, COMP_EXITED, COMP_IS_BINARY, COMP_READY, COMP_STARTED,
    COMP_THREAD_CAN_EXIT,
};
use dynlink::compartment::MONITOR_COMPARTMENT_ID;
use happylock::{LockCollection, RwLock, ThreadKey};
use monitor_api::{
    CompartmentFlags, RuntimeThreadControl, SharedCompConfig, TlsTemplateInfo, MONITOR_INSTANCE_ID,
};
use secgate::util::HandleMgr;
use thread::DEFAULT_STACK_SIZE;
use twizzler_abi::{syscall::sys_thread_exit, upcall::UpcallFrame};
use twizzler_rt_abi::{
    error::{GenericError, TwzError},
    object::{MapFlags, ObjID},
    thread::ThreadSpawnArgs,
};

use self::{
    compartment::{CompConfigObject, CompartmentHandle, RunComp},
    space::{MapHandle, MapInfo, Unmapper},
    thread::{ManagedThread, ThreadCleaner},
};
use crate::{gates::MonitorCompControlCmd, init::InitDynlinkContext};

pub(crate) mod compartment;
pub mod library;
pub(crate) mod space;
pub mod stat;
pub(crate) mod thread;

/// A security monitor instance. All monitor logic is implemented as methods for this type.
/// We split the state into the following components: 'space', managing the virtual memory space and
/// mapping objects, 'thread_mgr', which manages all threads owned by the monitor (typically, all
/// threads started by compartments), 'compartments', which manages compartment state, and
/// 'dynlink', which contains the dynamic linker state. The unmapper allows for background unmapping
/// and cleanup of objects and handles. There are also two hangle managers, for the monitor to hand
/// out handles to libraries and compartments to callers.
pub struct Monitor {
    locks: LockCollection<MonitorLocks<'static>>,
    unmapper: OnceLock<Unmapper>,
    /// Management of address space.
    pub space: &'static Mutex<space::Space>,
    /// Management of all threads.
    pub thread_mgr: &'static RwLock<thread::ThreadMgr>,
    /// Management of compartments.
    pub comp_mgr: &'static RwLock<compartment::CompartmentMgr>,
    /// Dynamic linker state.
    pub dynlink: &'static RwLock<&'static mut dynlink::context::Context>,
    /// Open handles to libraries.
    pub library_handles: &'static RwLock<HandleMgr<library::LibraryHandle>>,
    /// Open handles to compartments.
    pub _compartment_handles: &'static RwLock<HandleMgr<CompartmentHandle>>,
}

// We allow locking individually, using eg mon.space.write(key), or locking the collection for more
// complex operations that touch multiple pieces of state.
type MonitorLocks<'a> = (
    &'a RwLock<thread::ThreadMgr>,
    &'a RwLock<compartment::CompartmentMgr>,
    &'a RwLock<&'static mut dynlink::context::Context>,
    &'a RwLock<HandleMgr<library::LibraryHandle>>,
    &'a RwLock<HandleMgr<CompartmentHandle>>,
);

impl Monitor {
    /// Start the background threads for the monitor instance. Must be done only once the monitor
    /// has been initialized.
    pub fn start_background_threads(&self) {
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

        let ctx = init.get_safe_context();
        // Build our TLS region, and create a template for the monitor compartment.
        let super_tls = ctx
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
        let cc_handle = space
            .safe_create_and_map_runtime_object(
                MONITOR_INSTANCE_ID,
                MapFlags::READ | MapFlags::WRITE,
            )
            .unwrap();
        let stack_handle = space
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
            CompConfigObject::new(cc_handle, monitor_scc),
            (CompartmentFlags::READY | CompartmentFlags::STARTED).bits(),
            StackObject::new(stack_handle, DEFAULT_STACK_SIZE).unwrap(),
            0, /* doesn't matter -- we won't be starting a main thread for this compartment in
                * the normal way */
            &[],
        ));

        // Allocate and leak all the locks (they are global and eternal, so we can do this to safely
        // and correctly get &'static lifetime)
        let space = Box::leak(Box::new(Mutex::new(space)));
        let thread_mgr = Box::leak(Box::new(RwLock::new(thread::ThreadMgr::default())));
        let comp_mgr = Box::leak(Box::new(RwLock::new(comp_mgr)));
        let dynlink = Box::leak(Box::new(RwLock::new(ctx)));
        let library_handles = Box::leak(Box::new(RwLock::new(HandleMgr::new(None))));
        let compartment_handles = Box::leak(Box::new(RwLock::new(HandleMgr::new(None))));

        // Okay to call try_new here, since it's not many locks and only happens once.
        Self {
            locks: LockCollection::try_new((
                &*thread_mgr,
                &*comp_mgr,
                &*dynlink,
                &*library_handles,
                &*compartment_handles,
            ))
            .unwrap(),
            unmapper: OnceLock::new(),
            space,
            thread_mgr,
            comp_mgr,
            dynlink,
            library_handles,
            _compartment_handles: compartment_handles,
        }
    }

    /// Start a managed monitor thread.
    #[tracing::instrument(skip(self, main), level = tracing::Level::DEBUG)]
    pub fn start_thread(&self, main: Box<dyn FnOnce()>) -> Result<ManagedThread, TwzError> {
        let key = ThreadKey::get().unwrap();
        let locks = &mut *self.locks.lock(key);

        let monitor_dynlink_comp = locks.2.get_compartment_mut(MONITOR_COMPARTMENT_ID).unwrap();
        locks.0.start_thread(monitor_dynlink_comp, main, None)
    }

    /// Spawn a thread into a given compartment, using initial thread arguments.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn spawn_compartment_thread(
        &self,
        instance: ObjID,
        args: ThreadSpawnArgs,
        stack_ptr: usize,
        thread_ptr: usize,
    ) -> Result<ObjID, TwzError> {
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
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_comp_config(&self, sctx: ObjID) -> Result<*const SharedCompConfig, TwzError> {
        let comps = self.comp_mgr.write(ThreadKey::get().unwrap());
        Ok(comps.get(sctx)?.comp_config_ptr())
    }

    /// Map an object into a given compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn map_object(&self, sctx: ObjID, info: MapInfo) -> Result<MapHandle, TwzError> {
        let handle = self.space.lock().unwrap().map(info)?;

        let mut comp_mgr = self.comp_mgr.write(ThreadKey::get().unwrap());
        let rc = comp_mgr.get_mut(sctx)?;
        let handle = rc.map_object(info, handle)?;
        Ok(handle)
    }

    /// Map a pair of objects into a given compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn map_pair(
        &self,
        sctx: ObjID,
        info: MapInfo,
        info2: MapInfo,
    ) -> Result<(MapHandle, MapHandle), TwzError> {
        let (handle, handle2) = self.space.lock().unwrap().map_pair(info, info2)?;

        let mut comp_mgr = self.comp_mgr.write(ThreadKey::get().unwrap());
        let rc = comp_mgr.get_mut(sctx)?;
        let handle = rc.map_object(info, handle)?;
        let handle2 = rc.map_object(info2, handle2)?;
        Ok((handle, handle2))
    }

    /// Unmap an object from a given compartmen.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn unmap_object(&self, sctx: ObjID, info: MapInfo) {
        let Some(key) = ThreadKey::get() else {
            tracing::warn!("todo: recursive locked unmap");
            return;
        };

        let mut comp_mgr = self.comp_mgr.write(key);
        if let Ok(comp) = comp_mgr.get_mut(sctx) {
            let handle = comp.unmap_object(info);
            drop(comp_mgr);
            drop(handle);
        }
    }

    /// Get the object ID for this compartment-thread's simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_thread_simple_buffer(&self, sctx: ObjID, thread: ObjID) -> Result<ObjID, TwzError> {
        let mut locks = self.locks.lock(ThreadKey::get().unwrap());
        let (_, ref mut comps, _, _, _) = *locks;
        let rc = comps.get_mut(sctx)?;
        let pt = rc.get_per_thread(thread);
        pt.simple_buffer_id().ok_or(GenericError::Internal.into())
    }

    /// Write bytes to this per-compartment thread's simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn _write_thread_simple_buffer(
        &self,
        sctx: ObjID,
        thread: ObjID,
        bytes: &[u8],
    ) -> Result<usize, TwzError> {
        let mut locks = self.locks.lock(ThreadKey::get().unwrap());
        let (_, ref mut comps, _, _, _) = *locks;
        let rc = comps.get_mut(sctx)?;
        let pt = rc.get_per_thread(thread);
        Ok(pt.write_bytes(bytes))
    }

    /// Read bytes from this per-compartment thread's simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn read_thread_simple_buffer(
        &self,
        sctx: ObjID,
        thread: ObjID,
        len: usize,
    ) -> Result<Vec<u8>, TwzError> {
        let mut locks = self.locks.lock(ThreadKey::get().unwrap());
        let (_, ref mut comps, _, _, _) = *locks;
        let rc = comps.get_mut(sctx)?;
        let pt = rc.get_per_thread(thread);
        Ok(pt.read_bytes(len))
    }

    /// Read the name of a compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn comp_name(&self, id: ObjID) -> Result<String, TwzError> {
        self.comp_mgr
            .read(ThreadKey::get().unwrap())
            .get(id)
            .map(|rc| rc.name.clone())
    }

    /// Perform a compartment control action on the calling compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn compartment_ctrl(
        &self,
        info: &secgate::GateCallInfo,
        cmd: MonitorCompControlCmd,
    ) -> Option<i32> {
        let src = info.source_context()?;
        tracing::debug!(
            "compartment ctrl from: {:?}, thread = {:?}: {:?}",
            src,
            info.thread_id(),
            cmd
        );
        match cmd {
            // Here, the thread has indicated that it has initialized the runtime (and run
            // constructors), and so is ready to call main. At this point, we make sure
            // no errors have occurred and that we should continue. Update flags to
            // ready via compare-and-swap to ensure no one has set an error flag, and
            // return. If this compartment is a binary, then return None so the runtime will call
            // main. Otherwise return Some(SUCCESS) so that the runtime immediately
            // calls the post-main hook.
            MonitorCompControlCmd::RuntimeReady => loop {
                let state = self.load_compartment_flags(src);
                if state & COMP_STARTED == 0
                    || state & COMP_DESTRUCTED != 0
                    || state & COMP_EXITED != 0
                {
                    tracing::warn!(
                        "runtime main thread {} encountered invalid compartment {} state: {}",
                        info.thread_id(),
                        src,
                        state
                    );
                    sys_thread_exit(127);
                }

                if self.update_compartment_flags(src, |state| Some(state | COMP_READY)) {
                    tracing::debug!(
                        "runtime main thread reached compartment ready state in {}: {:x}",
                        self.comp_name(src)
                            .unwrap_or_else(|_| String::from("unknown")),
                        state
                    );
                    break if state & COMP_IS_BINARY == 0 {
                        Some(0)
                    } else {
                        None
                    };
                }
            },
            MonitorCompControlCmd::RuntimePostMain => {
                // First we want to check if we are a binary, and if so, we don't have to wait
                // around in here.
                loop {
                    if self.update_compartment_flags(src, |state| {
                        // Binaries can exit immediately. All future cross-compartment calls fail.
                        if state & COMP_IS_BINARY != 0 {
                            Some(state | COMP_THREAD_CAN_EXIT)
                        } else {
                            Some(state)
                        }
                    }) {
                        tracing::debug!(
                            "runtime main thread reached compartment post-main state in {}",
                            self.comp_name(src)
                                .unwrap_or_else(|_| String::from("unknown"))
                        );
                        break;
                    }
                }
                // Wait until we are allowed to exit (no one has a living, callable reference to us,
                // or we are a binary), ant then set the destructed flag and return.
                loop {
                    let flags = self.load_compartment_flags(src);
                    if flags & COMP_THREAD_CAN_EXIT != 0
                        && self.update_compartment_flags(src, |state| Some(state | COMP_DESTRUCTED))
                    {
                        tracing::debug!(
                            "runtime main thread destructing in {}",
                            self.comp_name(src)
                                .unwrap_or_else(|_| String::from("unknown"))
                        );
                        break None;
                    }
                    self.wait_for_compartment_state_change(src, flags);
                }
            }
        }
    }

    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn set_nameroot(&self, _info: &secgate::GateCallInfo, root: ObjID) -> Result<(), TwzError> {
        crate::dlengine::set_naming(root)
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
