use std::{collections::HashMap, mem::MaybeUninit, sync::Arc};

use dynlink::tls::TlsRegion;
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{sys_spawn, sys_thread_exit, ThreadSyncSleep, UpcallTargetSpawnOption},
    thread::{ExecutionState, ThreadRepr},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_runtime_api::{ObjID, SpawnError};

use super::space::MapHandle;
use crate::api::MONITOR_INSTANCE_ID;

mod cleaner;

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
    cleaner: cleaner::ThreadCleaner,
}

impl ThreadMgr {
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

    fn do_spawn(start: unsafe extern "C" fn(usize) -> !, arg: usize) -> Result<Self, SpawnError> {
        todo!()
    }

    /// Start a thread, running the provided Box'd closure. The thread will be running in
    /// monitor-mode, and will have no connection to any compartment.
    pub fn start_thread(main: Box<dyn FnOnce()>) -> Result<Self, SpawnError> {
        let main_addr = Box::into_raw(Box::new(main)) as usize;
        unsafe extern "C" fn managed_thread_entry(main: usize) -> ! {
            {
                let main = Box::from_raw(main as *mut Box<dyn FnOnce()>);
                main();
            }

            sys_thread_exit(0);
        }

        Self::do_spawn(managed_thread_entry, main_addr)
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
