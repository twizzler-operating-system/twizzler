use std::{
    collections::HashMap,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{Arc, OnceLock},
};

use dynlink::{compartment::Compartment, tls::TlsRegion};
use monitor_api::{RuntimeThreadControl, MONITOR_INSTANCE_ID};
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{sys_spawn, sys_thread_exit, ThreadSyncSleep, UpcallTargetSpawnOption},
    thread::{ExecutionState, ThreadRepr},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_rt_abi::{
    error::{GenericError, TwzError},
    object::{MapFlags, ObjID},
};

use super::{
    get_monitor,
    space::{MapHandle, MapInfo},
};
use crate::{gates::ThreadMgrStats, mon::space::Space};

mod cleaner;
pub(crate) use cleaner::ThreadCleaner;

/// Stack size for the supervisor upcall stack.
pub const SUPER_UPCALL_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB
/// Default stack size for the user stack.
pub const DEFAULT_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB
/// Stack minimium alignment.
pub const STACK_SIZE_MIN_ALIGN: usize = 0x1000; // 4K

/// Manages all threads owned by the monitor. Typically, this is all threads.
/// Threads are spawned here and tracked in the background by a [cleaner::ThreadCleaner]. The thread
/// cleaner detects when a thread has exited and performs any final thread cleanup logic.
pub struct ThreadMgr {
    all: HashMap<ObjID, ManagedThread>,
    cleaner: OnceLock<cleaner::ThreadCleaner>,
    next_id: u32,
    id_stack: Vec<u32>,
}

impl Default for ThreadMgr {
    fn default() -> Self {
        Self {
            all: HashMap::default(),
            cleaner: OnceLock::new(),
            next_id: 1,
            id_stack: Vec::new(),
        }
    }
}

struct IdDropper<'a> {
    mgr: &'a mut ThreadMgr,
    id: u32,
}

impl<'a> IdDropper<'a> {
    pub fn freeze(self) -> u32 {
        let id = self.id;
        std::mem::forget(self);
        id
    }
}

impl<'a> Drop for IdDropper<'a> {
    fn drop(&mut self) {
        self.mgr.release_super_tid(self.id);
    }
}

impl ThreadMgr {
    pub(super) fn set_cleaner(&mut self, cleaner: cleaner::ThreadCleaner) {
        self.cleaner.set(cleaner).ok().unwrap();
    }

    fn next_super_tid(&mut self) -> IdDropper<'_> {
        let id = self.id_stack.pop().unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        });
        IdDropper { mgr: self, id }
    }

    fn release_super_tid(&mut self, id: u32) {
        self.id_stack.push(id);
    }

    fn do_remove(&mut self, thread: &ManagedThread) {
        self.all.remove(&thread.id);
        self.release_super_tid(thread.super_tid);
        if let Some(cleaner) = self.cleaner.get() {
            cleaner.untrack(thread.id);
        }
    }

    pub fn stat(&self) -> ThreadMgrStats {
        ThreadMgrStats {
            nr_threads: self.all.len(),
        }
    }

    unsafe fn spawn_thread(
        start: usize,
        super_stack_start: usize,
        super_thread_pointer: usize,
        arg: usize,
    ) -> Result<ObjID, TwzError> {
        let upcall_target = UpcallTarget::new(
            None,
            Some(twizzler_rt_abi::arch::__twz_rt_upcall_entry),
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
    }

    fn do_spawn(
        &mut self,
        monitor_dynlink_comp: &mut Compartment,
        start: unsafe extern "C" fn(usize) -> !,
        arg: usize,
        main_thread_comp: Option<ObjID>,
    ) -> Result<ManagedThread, TwzError> {
        let super_tls = monitor_dynlink_comp
            .build_tls_region(RuntimeThreadControl::default(), |layout| unsafe {
                NonNull::new(std::alloc::alloc_zeroed(layout))
            })
            .map_err(|_| GenericError::Internal)?;
        let super_tid = self.next_super_tid().freeze();
        unsafe {
            let tcb = super_tls.get_thread_control_block::<RuntimeThreadControl>();
            (*tcb).runtime_data.set_id(super_tid);
        }
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
        let repr = Space::map(
            &get_monitor().space,
            MapInfo {
                id,
                flags: MapFlags::READ,
            },
        )
        .unwrap();
        Ok(Arc::new(ManagedThreadInner {
            id,
            super_tid,
            repr: ManagedThreadRepr::new(repr),
            _super_stack: super_stack,
            _super_tls: super_tls,
            main_thread_comp,
        }))
    }

    /// Start a thread, running the provided Box'd closure. The thread will be running in
    /// monitor-mode, and will have no connection to any compartment.
    pub fn start_thread(
        &mut self,
        monitor_dynlink_comp: &mut Compartment,
        main: Box<dyn FnOnce()>,
        main_thread_comp: Option<ObjID>,
    ) -> Result<ManagedThread, TwzError> {
        let main_addr = Box::into_raw(Box::new(main)) as usize;
        unsafe extern "C" fn managed_thread_entry(main: usize) -> ! {
            {
                let main = Box::from_raw(main as *mut Box<dyn FnOnce()>);
                main();
            }

            sys_thread_exit(0);
        }

        let mt = self.do_spawn(
            monitor_dynlink_comp,
            managed_thread_entry,
            main_addr,
            main_thread_comp,
        );
        if let Ok(ref mt) = mt {
            if let Some(cleaner) = self.cleaner.get() {
                cleaner.track(mt.clone());
            }
        }
        mt
    }
}

/// Internal managed thread data.
pub struct ManagedThreadInner {
    /// The ID of the thread.
    pub id: ObjID,
    pub super_tid: u32,
    /// The thread repr.
    pub(crate) repr: ManagedThreadRepr,
    _super_stack: Box<[MaybeUninit<u8>]>,
    _super_tls: TlsRegion,
    pub main_thread_comp: Option<ObjID>,
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
