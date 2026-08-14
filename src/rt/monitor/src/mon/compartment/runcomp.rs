use std::{
    alloc::Layout,
    collections::HashMap,
    ffi::{CStr, CString},
    ptr::{addr_of, NonNull},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use dynlink::{compartment::CompartmentId, context::Context};
use monitor_api::{
    CompartmentFlags, RuntimeThreadControl, SharedCompConfig, ThreadInfo, TlsTemplateInfo,
};
use secgate::util::SimpleBuffer;
use talc::{ErrOnOom, Talc};
use twizzler_abi::{
    syscall::{
        DeleteFlags, ObjectControlCmd, ThreadSync, ThreadSyncFlags, ThreadSyncOp,
        ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
    },
    upcall::{ResumeFlags, UpcallData, UpcallFrame},
    write_note,
};
use twizzler_rt_abi::{
    core::{CompartmentInitInfo, CtorSet, InitInfoPtrs, RuntimeInfo, RUNTIME_INIT_COMP},
    error::TwzError,
    object::{MapFlags, ObjID},
};

use super::{compconfig::CompConfigObject, compthread::CompThread, StackObject};
use crate::mon::{
    get_monitor,
    space::{MapHandle, MapInfo, Space},
    thread::ThreadMgr,
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
    /// Behind its own lock so mapping does not need `&mut RunComp`: the monitor can then reach a
    /// compartment through a *read* of the compartment manager, letting maps into different
    /// compartments run concurrently, and maps into this one serialize only on a HashMap insert
    /// rather than on the manager's global write lock.
    mapped_objects: Mutex<HashMap<MapInfo, MapHandle>>,
    flags: Box<AtomicU64>,
    /// Behind its own lock, and each entry behind its own lock, for two separate reasons.
    ///
    /// A `PerThread`'s buffer is only ever touched by the thread that owns it -- every gate passes
    /// its own `info.thread_id()` -- so the inner `Mutex` is uncontended by construction. It
    /// exists because that invariant is not visible to the compiler, and because the thread
    /// cleaner needs a defined way to reach an entry.
    ///
    /// The *map*, by contrast, is genuinely shared: `spawn_compartment_thread` inserts on behalf
    /// of a thread other than the caller, `ThreadCleaner` removes, and `main_thread_exited`
    /// iterates. An insert can rehash and relocate every value, which is what made a bare
    /// `&mut PerThread` require `&mut RunComp` -- and thence the monitor's whole exclusive
    /// `LockCollection` -- for what is otherwise a memcpy into this thread's own object.
    per_thread: Mutex<HashMap<ObjID, Arc<Mutex<PerThread>>>>,
    /// Answers derived from this compartment's dynlink state, cached so the paths that need them
    /// do not have to take the dynlink lock.
    ///
    /// `gate_address_named` and `get_compartment_info` were reading `dynlink` for facts that do
    /// not change unless a library is loaded into this compartment: a gate's implementation
    /// address, and how many libraries there are. Both took a *read* of the whole lock
    /// collection to get them, and `RunCompLoader::new` holds `dynlink` for a **write** across
    /// a median 31 ms (`sysperf.md` round 8), so every such call stalled behind any
    /// compartment load in the system. Worse for `gate_address_named`, which is on the
    /// dynamic-gate call path and scanned every library's every gate for a name match on each
    /// call.
    ///
    /// Behind their own locks for the same reason `per_thread` and `mapped_objects` are: reaching
    /// a compartment through a *read* of the manager, rather than needing `&mut RunComp` and
    /// thence the exclusive collection.
    ///
    /// Invalidated by [`Self::invalidate_dynlink_cache`] when a library is loaded into this
    /// compartment; a compartment that is torn down drops the whole `RunComp` with them.
    gate_cache: Mutex<HashMap<String, usize>>,
    /// 0 means "not computed yet" -- a live compartment always has at least one library.
    nr_libs: AtomicUsize,
    init_info: Option<(StackObject, usize, usize, Vec<CtorSet>)>,
    is_debugging: bool,
    pub(crate) use_count: u64,
    pub controller: Option<ObjID>,
}

impl RunComp {
    /// A gate address this compartment has resolved before.
    pub fn cached_gate(&self, name: &str) -> Option<usize> {
        self.gate_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .copied()
    }

    pub fn cache_gate(&self, name: &str, addr: usize) {
        self.gate_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), addr);
    }

    /// This compartment's library count, if it has been computed since the last load.
    pub fn cached_nr_libs(&self) -> Option<usize> {
        match self.nr_libs.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    pub fn cache_nr_libs(&self, n: usize) {
        self.nr_libs.store(n, Ordering::Relaxed);
    }

    /// Drop everything derived from dynlink state. Call after loading a library into this
    /// compartment: a new library can add gates and changes the library count.
    pub fn invalidate_dynlink_cache(&self) {
        self.gate_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.nr_libs.store(0, Ordering::Relaxed);
    }
}

impl Drop for RunComp {
    fn drop(&mut self) {
        // TODO: check if we need to do anything.
        let _ = twizzler_abi::syscall::sys_object_ctrl(
            self.instance,
            ObjectControlCmd::Delete(DeleteFlags::empty()),
            0,
            0,
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

        if let Some(handle) = &handle {
            write_note!(handle.id(), "comp-thread-sb:{}:{}", instance, _th);
        }

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
        main_entry: usize,
        ctors: &[CtorSet],
        is_debugging: bool,
        controller: Option<ObjID>,
        alloc: Talc<ErrOnOom>,
    ) -> Self {
        write_note!(instance, "comp:{}", name);
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
            mapped_objects: Mutex::new(HashMap::default()),
            flags: Box::new(AtomicU64::new(flags)),
            per_thread: Mutex::new(HashMap::new()),
            gate_cache: Mutex::new(HashMap::new()),
            nr_libs: AtomicUsize::new(0),
            init_info: Some((main_stack, entry, main_entry, ctors.to_vec())),
            use_count: 0,
            controller,
        }
    }

    /// Get per-thread data in this compartment, creating it if this thread has none yet.
    ///
    /// Returns an owned handle rather than a borrow: the caller drops the manager's lock before
    /// using the buffer, and the `Arc` is what keeps the buffer alive for that window -- not the
    /// `RunComp`, which a concurrent teardown may remove. (In practice a thread cannot be inside
    /// the monitor when its own compartment is torn down: `main_thread_exited` force-exits with
    /// `sys_thread_change_state_in_sctx` restricted to the instance, so the kernel holds the exit
    /// until the victim is back on its own code. The `Arc` means correctness does not rest on it.)
    pub fn get_per_thread(&self, id: ObjID) -> Arc<Mutex<PerThread>> {
        let instance = self.instance;
        // `PerThread::new` creates and maps an object under this lock. Deliberate: it runs once per
        // thread per compartment (8 times over a whole boot, measured), the lock is this
        // compartment's alone, and the alternative -- create outside, insert under -- wastes an
        // object whenever two threads race for the same entry.
        self.per_thread
            .lock()
            .unwrap()
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(PerThread::new(instance, id))))
            .clone()
    }

    /// Remove all per-thread data for a given thread.
    pub fn clean_per_thread_data(&self, id: ObjID) {
        // Dropped after the lock: the last `Arc` releasing a `MapHandle` sends to the unmapper.
        let _old = self.per_thread.lock().unwrap().remove(&id);
    }

    /// The threads this compartment has per-thread data for, plus any of `also` not already in it.
    ///
    /// Collected rather than iterated in place: the caller force-exits each one, and a syscall
    /// under this lock would block every gate call in the compartment behind it.
    pub fn thread_ids_including(&self, also: &[ObjID]) -> Vec<ObjID> {
        let pt = self.per_thread.lock().unwrap();
        pt.keys()
            .copied()
            .chain(also.iter().copied().filter(|t| !pt.contains_key(t)))
            .collect()
    }

    /// Map an object into this compartment.
    /// Deliberately writes no object note. A `map:<comp>` note cost a `format!` and a
    /// `sys_object_add_note` syscall per map, and both ran under the monitor's global `comp_mgr`
    /// write lock, serializing every mapping in the system behind a syscall.
    pub fn map_object(&self, info: MapInfo, handle: MapHandle) -> Result<MapHandle, TwzError> {
        // Dropped after the lock: a `MapHandle` reaching zero sends to the unmapper thread, which
        // has no business happening inside this compartment's map lock.
        let _old = self
            .mapped_objects
            .lock()
            .unwrap()
            .insert(info, handle.clone());
        Ok(handle)
    }

    /// Record a mapping the monitor made on this compartment's behalf, before the compartment has
    /// asked for it. Deliberately not `map_object`: an existing mapping is never clobbered.
    pub fn premap_object(&self, info: MapInfo, handle: MapHandle) {
        self.mapped_objects
            .lock()
            .unwrap()
            .entry(info)
            .or_insert(handle);
    }

    /// Unmap and object from this compartment.
    pub fn unmap_object(&self, info: MapInfo) -> Option<MapHandle> {
        let x = self.mapped_objects.lock().unwrap().remove(&info);
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
    pub fn until_change(&self, cur: u64) -> [ThreadSync; 2] {
        let ccp = self.comp_config_ptr();
        let ps = unsafe { addr_of!((*ccp).posted_signals) };
        [
            ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&*self.flags),
                cur,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            )),
            ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(ps),
                0,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            )),
        ]
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
        let (stack, entry, main_entry, ctors) = self.init_info.take().unwrap();
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
            let rtinfo = RuntimeInfo {
                flags: 0,
                kind: RUNTIME_INIT_COMP,
                args: args_in_comp_in_comp.cast(),
                argc,
                entry: main_entry,
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
            if main_entry != 0 { main_entry } else { entry },
            arg,
            suspend_on_start,
        ) {
            Ok(mt) => mt,
            Err(_) => {
                self.set_flag(COMP_EXITED);
                return None;
            }
        };
        write_note!(mt.thread.id, "thread:{}(main)", self.name);
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
            .lock()
            .unwrap()
            .keys()
            .filter(|t| **t != main.thread.id)
            .nth(n - 1)
            .map(|id| ThreadInfo { repr_id: *id })
    }

    pub fn main_thread(&self) -> &Option<CompThread> {
        &self.main
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
            // The kernel already prints the faulting rip and an unwound frame list, but every
            // address in it is a bare virtual address, which says nothing without knowing what is
            // mapped where. Dump this compartment's object map so those addresses can be
            // attributed to an object (and thence a library, via the libname map) after the fact.
            // This is the last thing that runs before the compartment is marked dead, so it is the
            // only chance to record it.
            tracing::warn!(
                "  fault ip {:#x} sp {:#x} bp {:#x}",
                frame.ip(),
                frame.sp(),
                frame.bp()
            );
            // Best effort, and never fatal: this runs on the fault path, possibly on the very
            // thread holding the lock, and the `set_flag` below must run either way or anyone in
            // `compartment_wait` blocks forever.
            match self.mapped_objects.try_lock() {
                Ok(mapped) => {
                    for (info, handle) in mapped.iter() {
                        let addrs = handle.addrs();
                        tracing::warn!(
                            "  mapped {} {:?} start {:#x} meta {:#x}",
                            info.id,
                            info.flags,
                            addrs.start,
                            addrs.meta
                        );
                    }
                }
                Err(_) => tracing::warn!("  (mapped-object list unavailable: lock held)"),
            }
            // The faulting thread is about to exit without ever reaching the normal exit paths, so
            // nothing else would mark this compartment dead. Without this, anyone in
            // `compartment_wait` (init, and the test runner behind it) blocks forever and the
            // fault surfaces as an orphaned-thread hang instead of a failed compartment.
            self.set_flag(COMP_EXITED);
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
