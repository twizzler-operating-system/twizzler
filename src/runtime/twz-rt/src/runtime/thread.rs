//! Implements thread management routines.

use std::{
    alloc::Layout,
    cell::UnsafeCell,
    collections::HashMap,
    panic::catch_unwind,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
};

use dynlink::tls::{Tcb, TlsRegion};
use lazy_static::lazy_static;
use tracing::debug;
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{
        sys_spawn, sys_thread_sync, sys_thread_yield, ThreadSpawnArgs, ThreadSpawnFlags,
        ThreadSync, ThreadSyncError, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
        ThreadSyncSleep, ThreadSyncWake,
    },
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_runtime_api::{
    CoreRuntime, JoinError, MapFlags, ObjectHandle, ObjectRuntime, SpawnError, ThreadRuntime,
    TlsIndex,
};

use crate::{monitor::get_monitor_actions, preinit_println};

use super::{ReferenceRuntime, OUR_RUNTIME};

const MIN_STACK_ALIGN: usize = 128;
const THREAD_NAME_MAX: usize = 128;
const THREAD_STARTED: u32 = 1;
pub struct RuntimeThreadControl {
    internal_lock: AtomicU32,
    flags: AtomicU32,
    id: u32,
    inner: std::cell::UnsafeCell<RuntimeThreadControlInner>,
}

pub struct RuntimeThreadControlInner {
    name: [u8; THREAD_NAME_MAX + 1],
}

impl RuntimeThreadControl {
    pub fn new() -> Self {
        Self {
            internal_lock: AtomicU32::default(),
            flags: AtomicU32::default(),
            id: 0,
            inner: UnsafeCell::new(RuntimeThreadControlInner {
                name: [0; THREAD_NAME_MAX + 1],
            }),
        }
    }

    fn write_lock(&self) {
        loop {
            let old = self.internal_lock.fetch_or(1, Ordering::Acquire);
            if old == 0 {
                break;
            }
        }
    }

    fn write_unlock(&self) {
        self.internal_lock.fetch_and(!1, Ordering::Release);
    }

    fn read_lock(&self) {
        loop {
            let old = self.internal_lock.fetch_add(2, Ordering::Acquire);
            if old > i32::MAX as u32 {
                OUR_RUNTIME.abort();
            }
            if old & 1 == 0 {
                break;
            }
        }
    }

    fn read_unlock(&self) {
        self.internal_lock.fetch_sub(2, Ordering::Release);
    }

    pub fn write_name(&self, name: &[u8]) {
        let name = if name.len() > THREAD_NAME_MAX {
            &name[0..THREAD_NAME_MAX]
        } else {
            name
        };
        unsafe {
            self.inner.get().as_mut().unwrap().name[0..name.len()].copy_from_slice(name);
        }
    }
}

pub struct InternalThread {
    repr_handle: ObjectHandle,
    stack_addr: usize,
    stack_size: usize,
    args_box: usize,
    id: u32,
    tls: TlsRegion,
}

impl InternalThread {
    fn repr(&self) -> &ThreadRepr {
        unsafe {
            (self.repr_handle.start.add(NULLPAGE_SIZE) as *const ThreadRepr)
                .as_ref()
                .unwrap()
        }
    }
}

impl Drop for InternalThread {
    fn drop(&mut self) {
        debug!("dropping InternalThread {}", self.id);
        unsafe {
            let alloc = OUR_RUNTIME.default_allocator();
            alloc.dealloc(
                self.stack_addr as *mut u8,
                Layout::from_size_align(self.stack_size, MIN_STACK_ALIGN).unwrap(),
            );
            alloc.dealloc(self.tls.alloc_base(), self.tls.alloc_layout());
            let _args = Box::from_raw(self.args_box as *mut u8);
            drop(_args);
        }
    }
}

struct ThreadManager {
    inner: Mutex<ThreadManagerInner>,
}

struct ThreadManagerInner {
    all_threads: HashMap<u32, InternalThread>,
    to_cleanup: Vec<InternalThread>,
    id_stack: Vec<u32>,
    next_id: u32,
}

fn with_current_thread<R, F: FnOnce(&RuntimeThreadControl) -> R>(f: F) -> R {
    let tp: &mut Tcb<RuntimeThreadControl> = unsafe {
        dynlink::tls::get_current_thread_control_block()
            .as_mut()
            .unwrap()
    };
    f(&tp.runtime_data)
}

lazy_static! {
    static ref THREAD_MGR: ThreadManager = ThreadManager {
        inner: Mutex::new(ThreadManagerInner {
            all_threads: HashMap::new(),
            to_cleanup: Vec::new(),
            id_stack: Vec::new(),
            next_id: 1,
        }),
    };
}

unsafe impl Send for ThreadManager {}
unsafe impl Sync for ThreadManager {}

// TODO: implement spawning and joining

extern "C" fn trampoline(arg: usize) -> ! {
    let code = catch_unwind(|| {
        with_current_thread(|cur| {
            // Needs an acq barrier here for the ID, but also a release for the flags.
            cur.flags.fetch_or(THREAD_STARTED, Ordering::SeqCst);
            debug!("thread {} started", cur.id);
        });
        let arg = unsafe {
            (arg as *const twizzler_runtime_api::ThreadSpawnArgs)
                .as_ref()
                .unwrap()
        };
        let entry: extern "C" fn(usize) = unsafe { core::mem::transmute(arg.start) };
        (entry)(arg.arg);
        0
    })
    .unwrap_or(101);
    twizzler_abi::syscall::sys_thread_exit(code);
}

impl ThreadRuntime for ReferenceRuntime {
    fn available_parallelism(&self) -> core::num::NonZeroUsize {
        twizzler_abi::syscall::sys_info().cpu_count()
    }

    fn futex_wait(
        &self,
        futex: &core::sync::atomic::AtomicU32,
        expected: u32,
        timeout: Option<core::time::Duration>,
    ) -> bool {
        // No need to wait if the value already changed.
        if futex.load(core::sync::atomic::Ordering::Relaxed) != expected {
            return true;
        }

        let r = sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual32(futex),
                expected as u64,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
            timeout,
        );

        !matches!(r, Err(ThreadSyncError::Timeout))
    }

    fn futex_wake(&self, futex: &core::sync::atomic::AtomicU32) -> bool {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            1,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
        // TODO
        false
    }

    fn futex_wake_all(&self, futex: &core::sync::atomic::AtomicU32) {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            usize::MAX,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
    }

    fn spawn(
        &self,
        args: twizzler_runtime_api::ThreadSpawnArgs,
    ) -> Result<u32, twizzler_runtime_api::SpawnError> {
        let args = Box::new(args);
        let tls: TlsRegion = get_monitor_actions()
            .allocate_tls_region()
            .ok_or(SpawnError::Other)?;
        let stack_raw = unsafe {
            OUR_RUNTIME
                .default_allocator()
                .alloc_zeroed(Layout::from_size_align(args.stack_size, MIN_STACK_ALIGN).unwrap())
        } as usize;

        // Take the thread management lock, so that when the new thread starts we cannot observe that thread
        // running without the management data being recorded.
        let mut inner = THREAD_MGR.inner.lock().unwrap();
        let id = inner.next_id();

        unsafe {
            tls.get_thread_control_block::<RuntimeThreadControl>()
                .as_mut()
                .unwrap()
                .runtime_data
                .id = id.id;
        }

        let stack_size = args.stack_size;
        let arg_raw = Box::into_raw(args) as usize;

        debug!(
            "spawning thread {} with stack {:x}, entry {:x}, and TLS {:x}",
            id.id,
            stack_raw,
            trampoline as usize,
            tls.get_thread_pointer_value(),
        );

        let thid = unsafe {
            sys_spawn(ThreadSpawnArgs {
                entry: trampoline as usize,
                stack_base: stack_raw,
                stack_size: stack_size,
                tls: tls.get_thread_pointer_value(),
                arg: arg_raw,
                flags: ThreadSpawnFlags::empty(),
                vm_context_handle: None,
            })
        }
        .map_err(|_| twizzler_runtime_api::SpawnError::Other /* TODO */)?;

        let thread_repr_obj = self
            .map_object(thid.as_u128(), MapFlags::READ | MapFlags::WRITE)
            .map_err(|_| SpawnError::Other /* TODO */)?;

        let thread = InternalThread {
            id: id.freeze(),
            tls,
            repr_handle: thread_repr_obj,
            stack_addr: stack_raw,
            stack_size,
            args_box: arg_raw,
        };
        let id = thread.id;
        inner.all_threads.insert(thread.id, thread);

        Ok(id)
    }

    fn yield_now(&self) {
        sys_thread_yield()
    }

    fn set_name(&self, name: &std::ffi::CStr) {
        with_current_thread(|cur| cur.write_name(name.to_bytes()))
    }

    fn sleep(&self, duration: std::time::Duration) {
        let _ = sys_thread_sync(&mut [], Some(duration));
    }

    fn join(&self, id: u32, timeout: Option<std::time::Duration>) -> Result<(), JoinError> {
        debug!("joining on thread {} with timeout {:?}", id, timeout);
        let repr = THREAD_MGR
            .inner
            .lock()
            .unwrap()
            .all_threads
            .get(&id)
            .ok_or(JoinError::LookupError)?
            .repr_handle
            .clone();
        let base =
            unsafe { (repr.start.add(NULLPAGE_SIZE) as *const ThreadRepr).as_ref() }.unwrap();
        loop {
            let (state, _code) = base.wait(timeout).ok_or(JoinError::Timeout)?;
            if state == ExecutionState::Exited {
                let mut inner = THREAD_MGR.inner.lock().unwrap();
                inner.prep_cleanup(id);
                inner.do_thread_gc();
                return Ok(());
            }
        }
    }

    fn tls_get_addr(&self, index: &TlsIndex) -> Option<*const u8> {
        let tp: &Tcb<()> = unsafe {
            dynlink::tls::get_current_thread_control_block()
                .as_ref()
                .expect("failed to find thread control block")
        };
        tp.get_addr(index)
    }
}

impl ThreadManagerInner {
    fn prep_cleanup(&mut self, id: u32) {
        if let Some(th) = self.all_threads.remove(&id) {
            self.to_cleanup.push(th);
        }
    }

    fn do_thread_gc(&mut self) {
        debug!(
            "starting thread GC round with {} dead threads",
            self.to_cleanup.len()
        );
        for th in self.to_cleanup.drain(..) {
            drop(th)
        }
    }

    fn scan_for_exited(&mut self) {
        for (_, th) in self
            .all_threads
            .extract_if(|_, th| th.repr().get_state() == ExecutionState::Exited)
        {
            debug!("found orphaned thread {}", th.id);
            self.to_cleanup.push(th);
        }
    }

    fn next_id(&mut self) -> IdDropper<'_> {
        let raw = self.id_stack.pop().unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        });
        IdDropper { tm: self, id: raw }
    }

    fn release_id(&mut self, id: u32) {
        self.id_stack.push(id)
    }
}

struct IdDropper<'a> {
    tm: &'a mut ThreadManagerInner,
    id: u32,
}

impl<'a> IdDropper<'a> {
    fn freeze(mut self) -> u32 {
        let id = self.id;
        self.id = 0;
        id
    }
}

impl<'a> Drop for IdDropper<'a> {
    fn drop(&mut self) {
        if self.id != 0 {
            self.tm.release_id(self.id)
        }
    }
}
