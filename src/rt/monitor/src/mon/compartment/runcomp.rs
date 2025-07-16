use std::{
    alloc::Layout,
    collections::HashMap,
    ffi::{CStr, CString},
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
};

use dynlink::{compartment::CompartmentId, context::Context};
use monitor_api::{CompartmentFlags, RuntimeThreadControl, SharedCompConfig, TlsTemplateInfo};
use secgate::util::SimpleBuffer;
use talc::{ErrOnOom, Talc};
use twizzler_abi::{
    syscall::{
        DeleteFlags, ObjectControlCmd, ThreadSync, ThreadSyncFlags, ThreadSyncOp,
        ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
    },
    upcall::{ResumeFlags, UpcallData, UpcallFrame},
};
use twizzler_rt_abi::{
    core::{CompartmentInitInfo, CtorSet, InitInfoPtrs, RuntimeInfo, RUNTIME_INIT_COMP},
    error::TwzError,
    object::{MapFlags, ObjID},
};

use super::{compconfig::CompConfigObject, compthread::CompThread, StackObject};
use crate::{
    gates::ThreadInfo,
    mon::{
        get_monitor,
        space::{MapHandle, MapInfo, Space},
        thread::ThreadMgr,
    },
};

/// Compartment is ready (loaded, reloacated, runtime started and ctors run).
pub const COMP_READY: u64 = CompartmentFlags::READY.bits();
/// Compartment is a binary, not a library.
pub const COMP_IS_BINARY: u64 = CompartmentFlags::IS_BINARY.bits();
/// Compartment runtime thread may exit.
pub const COMP_THREAD_CAN_EXIT: u64 = CompartmentFlags::THREAD_CAN_EXIT.bits();
/// Compartment thread has been started once.
pub const COMP_STARTED: u64 = CompartmentFlags::STARTED.bits();
/// Compartment destructors have run.
pub const COMP_DESTRUCTED: u64 = CompartmentFlags::DESTRUCTED.bits();
/// Compartment thread has exited.
pub const COMP_EXITED: u64 = CompartmentFlags::EXITED.bits();

/// A runnable or running compartment.
pub struct RunComp {
    /// The security context for this compartment.
    pub sctx: ObjID,
    /// The instance of the security context.
    pub instance: ObjID,
    /// The name of this compartment.
    pub name: String,
    /// The dynlink ID of this compartment.
    pub compartment_id: CompartmentId,
    main: Option<CompThread>,
    pub deps: Vec<ObjID>,
    comp_config_object: CompConfigObject,
    alloc: Talc<ErrOnOom>,
    mapped_objects: HashMap<MapInfo, MapHandle>,
    flags: Box<AtomicU64>,
    per_thread: HashMap<ObjID, PerThread>,
    init_info: Option<(StackObject, usize, Vec<CtorSet>)>,
    is_debugging: bool,
    pub(crate) use_count: u64,
}

impl Drop for RunComp {
    fn drop(&mut self) {
        // TODO: check if we need to do anything.
        let _ = twizzler_abi::syscall::sys_object_ctrl(
            self.instance,
            ObjectControlCmd::Delete(DeleteFlags::empty()),
        )
        .inspect_err(|e| tracing::warn!("failed to delete instance on RunComp drop: {}", e));
    }
}

impl core::fmt::Debug for RunComp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunComp")
            .field("sctx", &self.sctx)
            .field("instance", &self.instance)
            .field("name", &self.name)
            .field("deps", &self.deps)
            .field("usecount", &self.use_count)
            .field("dynlink_id", &self.compartment_id)
            .finish_non_exhaustive()
    }
}

/// Per-thread data in a compartment.
pub struct PerThread {
    simple_buffer: Option<(SimpleBuffer, MapHandle)>,
}

impl PerThread {
    /// Create a new PerThread. Note that this must succeed, so any allocation failures must be
    /// handled gracefully. This means that if the thread fails to allocate a simple buffer, it
    /// will just forego having one. This may cause a failure down the line, but it's the best we
    /// can do without panicing.
    fn new(instance: ObjID, _th: ObjID) -> Self {
        let handle = Space::safe_create_and_map_runtime_object(
            &get_monitor().space,
            instance,
            MapFlags::READ | MapFlags::WRITE,
        )
        .ok();

        Self {
            simple_buffer: handle
                .map(|handle| (SimpleBuffer::new(unsafe { handle.object_handle() }), handle)),
        }
    }

    /// Write bytes into this compartment-thread's simple buffer.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> usize {
        self.simple_buffer
            .as_mut()
            .map(|sb| sb.0.write(bytes))
            .unwrap_or(0)
    }

    /// Read bytes from this compartment-thread's simple buffer.
    pub fn read_bytes(&mut self, len: usize) -> Vec<u8> {
        let mut v = vec![0; len];
        let readlen = self
            .simple_buffer
            .as_mut()
            .map(|sb| sb.0.read(&mut v))
            .unwrap_or(0);
        v.truncate(readlen);
        v
    }

    /// Get the Object ID of this compartment thread's simple buffer.
    pub fn simple_buffer_id(&self) -> Option<ObjID> {
        Some(self.simple_buffer.as_ref()?.0.handle().id())
    }
}

impl RunComp {
    /// Build a new runtime compartment.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sctx: ObjID,
        instance: ObjID,
        name: String,
        compartment_id: CompartmentId,
        deps: Vec<ObjID>,
        comp_config_object: CompConfigObject,
        flags: u64,
        main_stack: StackObject,
        entry: usize,
        ctors: &[CtorSet],
        is_debugging: bool,
    ) -> Self {
        let mut alloc = Talc::new(ErrOnOom);
        unsafe { alloc.claim(comp_config_object.alloc_span()).unwrap() };
        Self {
            sctx,
            is_debugging,
            instance,
            name,
            compartment_id,
            main: None,
            deps,
            comp_config_object,
            alloc,
            mapped_objects: HashMap::default(),
            flags: Box::new(AtomicU64::new(flags)),
            per_thread: HashMap::new(),
            init_info: Some((main_stack, entry, ctors.to_vec())),
            use_count: 0,
        }
    }

    /// Get per-thread data in this compartment.
    pub fn get_per_thread(&mut self, id: ObjID) -> &mut PerThread {
        self.per_thread
            .entry(id)
            .or_insert_with(|| PerThread::new(self.instance, id))
    }

    /// Remove all per-thread data for a given thread.
    pub fn clean_per_thread_data(&mut self, id: ObjID) {
        self.per_thread.remove(&id);
    }

    /// Map an object into this compartment.
    pub fn map_object(&mut self, info: MapInfo, handle: MapHandle) -> Result<MapHandle, TwzError> {
        self.mapped_objects.insert(info, handle.clone());
        Ok(handle)
    }

    /// Unmap and object from this compartment.
    pub fn unmap_object(&mut self, info: MapInfo) -> Option<MapHandle> {
        let x = self.mapped_objects.remove(&info);
        if x.is_none() {
            // TODO:: this happens occasionally, but it doesn't seem to be an issue?
            tracing::debug!(
                "tried to comp-unmap an object that was not mapped by compartment ({}): {:?}",
                self.name,
                info
            );
        }
        x
    }

    /// Get a pointer to the compartment config.
    pub fn comp_config_ptr(&self) -> *const SharedCompConfig {
        self.comp_config_object.get_comp_config()
    }

    /// Allocate some space in the compartment allocator, and initialize it.
    pub fn monitor_new<T: Copy + Sized>(&mut self, data: T) -> Result<*mut T, ()> {
        unsafe {
            let place: NonNull<T> = self.alloc.malloc(Layout::new::<T>())?.cast();
            place.as_ptr().write(data);
            Ok(place.as_ptr())
        }
    }

    /// Allocate some space in the compartment allocator for a slice, and initialize it.
    pub fn monitor_new_slice<T: Copy + Sized>(&mut self, data: &[T]) -> Result<*mut T, ()> {
        unsafe {
            let place = self.alloc.malloc(Layout::array::<T>(data.len()).unwrap())?;
            let slice = core::slice::from_raw_parts_mut(place.as_ptr() as *mut T, data.len());
            slice.copy_from_slice(data);
            Ok(place.as_ptr() as *mut T)
        }
    }

    /// Set a flag on this compartment, and wakeup anyone waiting on flag change.
    pub fn set_flag(&self, val: u64) {
        tracing::trace!("compartment {} set flag {:x}", self.name, val);
        self.flags.fetch_or(val, Ordering::SeqCst);
        self.notify_state_changed();
    }

    /// Set a flag on this compartment, and wakeup anyone waiting on flag change.
    pub fn cas_flag(&self, old: u64, new: u64) -> Result<u64, u64> {
        let r = self
            .flags
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst);
        if r.is_ok() {
            tracing::trace!("compartment {} cas flag {:x} -> {:x}", self.name, old, new);
            self.notify_state_changed();
        }
        r
    }

    pub fn notify_state_changed(&self) {
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&*self.flags),
                usize::MAX,
            ))],
            None,
        );
    }

    /// Check if a flag is set.
    pub fn has_flag(&self, flag: u64) -> bool {
        self.flags.load(Ordering::SeqCst) & flag != 0
    }

    /// Setup a [ThreadSyncSleep] for waiting until the flag is set. Returns None if the flag is
    /// already set.
    pub fn until_change(&self, cur: u64) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&*self.flags),
            cur,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    /// Get the raw flags bits for this RC.
    pub fn raw_flags(&self) -> u64 {
        self.flags.load(Ordering::SeqCst)
    }

    pub(crate) fn start_main_thread(
        &mut self,
        state: u64,
        tmgr: &mut ThreadMgr,
        dynlink: &mut Context,
        args: &[&CStr],
        env: &[&CStr],
        suspend_on_start: bool,
    ) -> Option<bool> {
        if self.has_flag(COMP_STARTED) {
            return Some(false);
        }
        let state = state & !COMP_STARTED;
        if self
            .flags
            .compare_exchange(
                state,
                state | COMP_STARTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return None;
        }

        tracing::debug!("starting main thread for compartment {}", self.name);
        debug_assert!(self.main.is_none());
        // Unwrap-Ok: we only take this once, when starting the main thread.
        let (stack, entry, ctors) = self.init_info.take().unwrap();
        for c in &ctors {
            tracing::info!("==> {:?}", c);
        }
        let mut build_init_info = || -> Option<_> {
            let comp_config_info =
                self.comp_config_object.get_comp_config() as *mut SharedCompConfig;
            let ctors_in_comp = self.monitor_new_slice(&ctors).ok()?;

            // TODO: unwrap
            let mut args_in_comp: Vec<_> = args
                .iter()
                .map(|arg| self.monitor_new_slice(arg.to_bytes_with_nul()).unwrap())
                .collect();

            if args_in_comp.len() == 0 {
                let cname = CString::new(self.name.as_bytes()).unwrap();
                args_in_comp = vec![self.monitor_new_slice(cname.as_bytes()).unwrap()];
            }
            let argc = args_in_comp.len();

            let mut envs_in_comp: Vec<_> = env
                .iter()
                .map(|arg| self.monitor_new_slice(arg.to_bytes_with_nul()).unwrap())
                .collect();

            args_in_comp.push(core::ptr::null_mut());
            envs_in_comp.push(core::ptr::null_mut());

            let args_in_comp_in_comp = self.monitor_new_slice(&args_in_comp).unwrap();
            let envs_in_comp_in_comp = self.monitor_new_slice(&envs_in_comp).unwrap();

            let comp_init_info = CompartmentInitInfo {
                ctor_set_array: ctors_in_comp,
                ctor_set_len: ctors.len(),
                comp_config_info: comp_config_info.cast(),
            };
            let comp_init_info_in_comp = self.monitor_new(comp_init_info).ok()?;
            // TODO: fill out argc and argv and envp
            let rtinfo = RuntimeInfo {
                flags: 0,
                kind: RUNTIME_INIT_COMP,
                args: args_in_comp_in_comp.cast(),
                argc,
                envp: envs_in_comp_in_comp.cast(),
                init_info: InitInfoPtrs {
                    comp: comp_init_info_in_comp,
                },
            };
            self.monitor_new(rtinfo).ok()
        };
        let arg = match build_init_info() {
            Some(arg) => arg as usize,
            None => {
                self.set_flag(COMP_EXITED);
                return None;
            }
        };
        if self.build_tls_template(dynlink).is_none() {
            self.set_flag(COMP_EXITED);
            return None;
        }

        let mt = match CompThread::new(
            tmgr,
            dynlink,
            stack,
            self.instance,
            Some(self.instance),
            entry,
            arg,
            suspend_on_start,
        ) {
            Ok(mt) => mt,
            Err(_) => {
                self.set_flag(COMP_EXITED);
                return None;
            }
        };
        self.main = Some(mt);
        self.notify_state_changed();

        Some(true)
    }

    fn build_tls_template(&mut self, dynlink: &mut Context) -> Option<()> {
        let region = dynlink
            .get_compartment_mut(self.compartment_id)
            .unwrap()
            .build_tls_region(RuntimeThreadControl::default(), |layout| {
                unsafe { self.alloc.malloc(layout) }.ok()
            })
            .ok()?;

        let template: TlsTemplateInfo = region.into();
        let tls_template = self.monitor_new(template).ok()?;

        let config = self.comp_config_object.read_comp_config();
        config.set_tls_template(tls_template);
        self.comp_config_object.write_config(config);
        Some(())
    }

    #[allow(dead_code)]
    pub fn read_error_code(&self) -> u64 {
        let Some(ref main) = self.main else {
            return 0;
        };
        main.thread.repr.get_repr().get_code()
    }

    pub fn get_nth_thread_info(&self, n: usize) -> Option<ThreadInfo> {
        let Some(ref main) = self.main else {
            return None;
        };
        if n == 0 {
            return Some(ThreadInfo {
                repr_id: main.thread.id,
            });
        }
        self.per_thread
            .keys()
            .filter(|t| **t != main.thread.id)
            .nth(n - 1)
            .map(|id| ThreadInfo { repr_id: *id })
    }

    pub fn upcall_handle(
        &self,
        frame: &mut UpcallFrame,
        info: &UpcallData,
    ) -> Result<Option<ResumeFlags>, TwzError> {
        let flags = if self.is_debugging {
            tracing::info!("got monitor upcall {:?} {:?}", frame, info);
            Some(ResumeFlags::SUSPEND)
        } else {
            tracing::warn!(
                "supervisor exception in {}, thread {}: {:?}",
                self.name,
                info.thread_id,
                info.info
            );
            None
        };
        Ok(flags)
    }

    pub(crate) fn inc_use_count(&mut self) {
        self.use_count += 1;
        tracing::trace!(
            "compartment {} inc use count -> {}",
            self.name,
            self.use_count
        );
    }

    pub(crate) fn dec_use_count(&mut self) -> bool {
        debug_assert!(self.use_count > 0);
        self.use_count -= 1;

        tracing::trace!(
            "compartment {} dec use count -> {}",
            self.name,
            self.use_count
        );
        let z = self.use_count == 0;
        if z {
            self.set_flag(COMP_THREAD_CAN_EXIT);
        }
        z
    }
}
