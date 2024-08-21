use std::{
    cell::OnceCell,
    collections::HashMap,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{Arc, OnceLock},
};

use dynlink::{compartment::Compartment, context::Context, tls::TlsRegion};
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{sys_spawn, sys_thread_exit, ThreadSyncSleep, UpcallTargetSpawnOption},
    thread::{ExecutionState, ThreadRepr},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_runtime_api::{MapFlags, ObjID, SpawnError};
use twz_rt::RuntimeThreadControl;

use super::space::{MapHandle, MapInfo, Space};
use crate::api::MONITOR_INSTANCE_ID;

mod cleaner;
pub(crate) use cleaner::ThreadCleaner;

/// Stack size for the supervisor upcall stack.
pub const SUPER_UPCALL_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB
/// Default stack size for the user stack.
pub const DEFAULT_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB
/// Stack minimium alignment.
pub const STACK_SIZE_MIN_ALIGN: usize = 0x1000; // 4K
/// TLS minimum alignment.
pub const DEFAULT_TLS_ALIGN: usize = 0x1000; // 4K

/// Manages all threads owned by the monitor. Typically, this is all threads.
/// Threads are spawned here and tracked in the background by a [cleaner::ThreadCleaner]. The thread
/// cleaner detects when a thread has exited and performs any final thread cleanup logic.
pub struct ThreadMgr {
    all: HashMap<ObjID, ManagedThread>,
    cleaner: OnceLock<cleaner::ThreadCleaner>,
}

impl Default for ThreadMgr {
    fn default() -> Self {
        Self {
            all: HashMap::default(),
            cleaner: OnceLock::new(),
        }
    }
}

impl ThreadMgr {
    pub(super) fn set_cleaner(&mut self, cleaner: cleaner::ThreadCleaner) {
        self.cleaner.set(cleaner).ok().unwrap();
    }

    fn do_remove(&mut self, thread: &ManagedThread) {
        self.all.remove(&thread.id);
    }

    unsafe fn spawn_thread(
        start: usize,
        super_stack_start: usize,
        super_thread_pointer: usize,
        arg: usize,
    ) -> Result<ObjID, SpawnError> {
        let upcall_target = UpcallTarget::new(
            None,
            Some(twz_rt::rr_upcall_entry),
            super_stack_start,
            SUPER_UPCALL_STACK_SIZE,
            super_thread_pointer,
            MONITOR_INSTANCE_ID,
            [UpcallOptions {
                flags: UpcallFlags::empty(),
                mode: UpcallMode::CallSuper,
            }; UpcallInfo::NR_UPCALLS],
        );

        sys_spawn(twizzler_abi::syscall::ThreadSpawnArgs {
            entry: start,
            stack_base: super_stack_start,
            stack_size: SUPER_UPCALL_STACK_SIZE,
            tls: super_thread_pointer,
            arg,
            flags: twizzler_abi::syscall::ThreadSpawnFlags::empty(),
            vm_context_handle: None,
            upcall_target: UpcallTargetSpawnOption::SetTo(upcall_target),
        })
        .map_err(|_| SpawnError::KernelError)
    }

    fn do_spawn(
        &mut self,
        space: &mut Space,
        monitor_dynlink_comp: &mut Compartment,
        start: unsafe extern "C" fn(usize) -> !,
        arg: usize,
    ) -> Result<ManagedThread, SpawnError> {
        let super_tls = monitor_dynlink_comp
            .build_tls_region(RuntimeThreadControl::default(), |layout| unsafe {
                NonNull::new(std::alloc::alloc_zeroed(layout))
            })
            .map_err(|_| SpawnError::Other)?;
        let super_thread_pointer = super_tls.get_thread_pointer_value();
        let super_stack = Box::new_zeroed_slice(SUPER_UPCALL_STACK_SIZE);
        let id = unsafe {
            Self::spawn_thread(
                start as *const () as usize,
                super_stack.as_ptr() as usize,
                super_thread_pointer,
                arg,
            )?
        };
        let repr = space
            .map(MapInfo {
                id,
                flags: MapFlags::READ,
            })
            .unwrap();
        Ok(Arc::new(ManagedThreadInner {
            id,
            repr: ManagedThreadRepr::new(repr),
            super_stack,
            super_tls,
        }))
    }

    /// Start a thread, running the provided Box'd closure. The thread will be running in
    /// monitor-mode, and will have no connection to any compartment.
    pub fn start_thread(
        &mut self,
        space: &mut Space,
        monitor_dynlink_comp: &mut Compartment,
        main: Box<dyn FnOnce()>,
    ) -> Result<ManagedThread, SpawnError> {
        let main_addr = Box::into_raw(Box::new(main)) as usize;
        unsafe extern "C" fn managed_thread_entry(main: usize) -> ! {
            {
                let main = Box::from_raw(main as *mut Box<dyn FnOnce()>);
                main();
            }

            sys_thread_exit(0);
        }

        self.do_spawn(space, monitor_dynlink_comp, managed_thread_entry, main_addr)
    }
}

/// Internal managed thread data.
pub struct ManagedThreadInner {
    /// The ID of the thread.
    pub id: ObjID,
    /// The thread repr.
    pub(crate) repr: ManagedThreadRepr,
    super_stack: Box<[MaybeUninit<u8>]>,
    super_tls: TlsRegion,
}

impl ManagedThreadInner {
    /// Check if this thread has exited.
    pub fn has_exited(&self) -> bool {
        self.repr.get_repr().get_state() == ExecutionState::Exited
    }

    /// Create a ThreadSyncSleep that will wait until the thread has exited.
    pub fn waitable_until_exit(&self) -> ThreadSyncSleep {
        self.repr.get_repr().waitable(ExecutionState::Exited)
    }
}

// Safety: TlsRegion is not changed, and points to only globally- and permanently-allocated data.
unsafe impl Send for ManagedThreadInner {}
unsafe impl Sync for ManagedThreadInner {}

impl core::fmt::Debug for ManagedThreadInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ManagedThread({})", self.id)
    }
}

impl Drop for ManagedThreadInner {
    fn drop(&mut self) {
        // TODO
        tracing::trace!("dropping ManagedThread {}", self.id);
    }
}

/// A thread managed by the monitor.
pub type ManagedThread = Arc<ManagedThreadInner>;

/// An owned handle to a thread's repr object.
pub(crate) struct ManagedThreadRepr {
    handle: MapHandle,
}

impl ManagedThreadRepr {
    fn new(handle: MapHandle) -> Self {
        Self { handle }
    }

    /// Get the thread representation structure for the associated thread.
    pub fn get_repr(&self) -> &ThreadRepr {
        let addr = self.handle.addrs().start + NULLPAGE_SIZE;
        unsafe { (addr as *const ThreadRepr).as_ref().unwrap() }
    }
}
