use std::{
    alloc::Layout,
    collections::HashMap,
    marker::PhantomData,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use dynlink::compartment::CompartmentId;
use monitor_api::SharedCompConfig;
use secgate::util::SimpleBuffer;
use talc::{ErrOnOom, Talc};
use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_runtime_api::{MapError, MapFlags, ObjID, ObjectHandle};

use super::{compconfig::CompConfigObject, compthread::CompThread};
use crate::mon::{
    compartment::compthread::StackObject,
    space::{MapHandle, MapInfo, Space},
    thread::DEFAULT_STACK_SIZE,
};

/// Compartment is ready (loaded, reloacated, runtime started and ctors run).
pub const COMP_READY: u64 = 0x1;
/// Compartment is a binary, not a library.
pub const COMP_IS_BINARY: u64 = 0x2;
/// Compartment runtime thread may exit.
pub const COMP_THREAD_CAN_EXIT: u64 = 0x4;
/// Compartment thread has been started once.
pub const COMP_STARTED: u64 = 0x8;
/// Compartment destructors have run.
pub const COMP_DESTRUCTED: u64 = 0x10;
/// Compartment thread has exited.
pub const COMP_EXITED: u64 = 0x20;

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
    /// The dependencies of this compartment.
    pub deps: Vec<ObjID>,
    comp_config_object: CompConfigObject,
    alloc: Talc<ErrOnOom>,
    mapped_objects: HashMap<MapInfo, MapHandle>,
    flags: Box<AtomicU64>,
    per_thread: HashMap<ObjID, PerThread>,
    main_stack: Option<StackObject>,
}

impl core::fmt::Debug for RunComp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunComp")
            .field("sctx", &self.sctx)
            .field("instance", &self.instance)
            .field("name", &self.name)
            .field("deps", &self.deps)
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
    fn new(instance: ObjID, th: ObjID, space: &mut Space) -> Self {
        let handle = space
            .safe_create_and_map_runtime_object(instance, MapFlags::READ | MapFlags::WRITE)
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
        Some(self.simple_buffer.as_ref()?.0.handle().id)
    }
}

impl RunComp {
    /// Build a new runtime compartment.
    pub fn new(
        sctx: ObjID,
        instance: ObjID,
        name: String,
        compartment_id: CompartmentId,
        deps: Vec<ObjID>,
        comp_config_object: CompConfigObject,
        flags: u64,
        main_stack: StackObject,
    ) -> Self {
        let mut alloc = Talc::new(ErrOnOom);
        unsafe { alloc.claim(comp_config_object.alloc_span()).unwrap() };
        Self {
            sctx,
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
            main_stack: Some(main_stack),
        }
    }

    /// Get per-thread data in this compartment.
    pub fn get_per_thread(&mut self, id: ObjID, space: &mut Space) -> &mut PerThread {
        self.per_thread
            .entry(id)
            .or_insert_with(|| PerThread::new(self.instance, id, space))
    }

    /// Remove all per-thread data for a given thread.
    pub fn clean_per_thread_data(&mut self, id: ObjID) {
        self.per_thread.remove(&id);
    }

    /// Map an object into this compartment.
    pub fn map_object(&mut self, info: MapInfo, handle: MapHandle) -> Result<MapHandle, MapError> {
        self.mapped_objects.insert(info, handle.clone());
        Ok(handle)
    }

    /// Unmap and object from this compartment.
    pub fn unmap_object(&mut self, info: MapInfo) {
        let _ = self.mapped_objects.remove(&info);
        // Unmapping handled by dropping
    }

    /// Read the compartment config.
    pub fn comp_config(&self) -> SharedCompConfig {
        self.comp_config_object.read_comp_config()
    }

    /// Get a pointer to the compartment config.
    pub fn comp_config_ptr(&self) -> *const SharedCompConfig {
        self.comp_config_object.get_comp_config()
    }

    /// Set the compartment config.
    pub fn set_comp_config(&mut self, scc: SharedCompConfig) {
        self.comp_config_object.write_config(scc)
    }

    /// Allocate some space in the compartment allocator, and initialize it.
    pub fn monitor_new<T: Copy + Sized + Send + Sync>(&mut self, data: T) -> Result<*mut T, ()> {
        unsafe {
            let place: NonNull<T> = self.alloc.malloc(Layout::new::<T>())?.cast();
            place.as_ptr().write(data);
            Ok(place.as_ptr() as *mut T)
        }
    }

    /// Allocate some space in the compartment allocator for a slice, and initialize it.
    pub fn monitor_new_slice<T: Copy + Sized + Send + Sync>(
        &mut self,
        data: &[T],
    ) -> Result<*mut T, ()> {
        unsafe {
            let place = self.alloc.malloc(Layout::array::<T>(data.len()).unwrap())?;
            let slice = core::slice::from_raw_parts_mut(place.as_ptr() as *mut T, data.len());
            slice.copy_from_slice(data);
            Ok(place.as_ptr() as *mut T)
        }
    }

    /// Set a flag on this compartment, and wakeup anyone waiting on flag change.
    pub fn set_flag(&self, val: u64) {
        self.flags.fetch_or(val, Ordering::SeqCst);
        self.notify_state_changed();
    }

    /// Set a flag on this compartment, and wakeup anyone waiting on flag change.
    pub fn cas_flag(&self, old: u64, new: u64) -> Result<u64, u64> {
        let r = self
            .flags
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst);
        if r.is_ok() {
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
    pub fn flag_waitable(&self, flag: u64) -> Option<ThreadSyncSleep> {
        let flags = self.flags.load(Ordering::SeqCst);
        if flags & flag == 0 {
            Some(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&*self.flags),
                flags,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))
        } else {
            None
        }
    }

    /// Get the raw flags bits for this RC.
    pub fn raw_flags(&self) -> u64 {
        self.flags.load(Ordering::SeqCst)
    }

    pub(crate) fn start_main_thread(&mut self, state: u64) -> Option<bool> {
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
        let mt = match CompThread::new(self.main_stack.take().unwrap(), self.instance, || todo!()) {
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
}
