//! Thread management routines, including spawn and join.

use std::{
    alloc::{GlobalAlloc, Layout},
    collections::BTreeMap,
};

use monitor_api::RuntimeThreadControl;
use tracing::trace;
use twizzler_abi::{
    object::{ObjID, NULLPAGE_SIZE},
    simple_mutex::Mutex,
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_rt_abi::{
    error::{ArgumentError, NamingError, TwzError},
    object::MapFlags,
    thread::ThreadSpawnArgs,
    Result,
};

use super::internal::InternalThread;
use crate::runtime::{
    thread::{
        tcb::{trampoline, TLS_GEN_MGR},
        MIN_STACK_ALIGN, THREAD_MGR,
    },
    ReferenceRuntime, OUR_RUNTIME,
};

pub(crate) struct ThreadManager {
    inner: Mutex<ThreadManagerInner>,
}

impl ThreadManager {
    pub(super) const fn new() -> Self {
        Self {
            inner: Mutex::new(ThreadManagerInner::new()),
        }
    }

    pub fn with_internal<R, F: FnOnce(&InternalThread) -> R>(&self, id: u32, f: F) -> Option<R> {
        let inner = self.inner.lock();
        Some(f(inner.all_threads.get(&id)?))
    }
}

#[derive(Default)]
struct ThreadManagerInner {
    all_threads: BTreeMap<u32, InternalThread>,
    // Threads that have exited, but we haven't cleaned up yet.
    to_cleanup: Vec<InternalThread>,
    // Basic unique-ID system.
    id_stack: Vec<u32>,
    next_id: u32,
}

unsafe impl Send for ThreadManager {}
unsafe impl Sync for ThreadManager {}

impl ThreadManagerInner {
    const fn new() -> Self {
        Self {
            next_id: 1,
            all_threads: BTreeMap::new(),
            to_cleanup: vec![],
            id_stack: vec![],
        }
    }

    fn prep_cleanup(&mut self, id: u32) {
        if let Some(th) = self.all_threads.remove(&id) {
            self.to_cleanup.push(th);
        }
    }

    fn do_thread_gc(&mut self) {
        trace!(
            "starting thread GC round with {} dead threads",
            self.to_cleanup.len()
        );
        for th in self.to_cleanup.drain(..) {
            drop(th)
        }
    }

    fn scan_for_exited_except(&mut self, id: u32) {
        for (_, th) in self
            .all_threads
            .extract_if(|_, th| th.id != id && th.repr().get_state() == ExecutionState::Exited)
        {
            trace!("found orphaned thread {}", th.id);
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

// Makes spawn easier to read, as it'll auto-cleanup IDs on failure.
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

impl ReferenceRuntime {
    pub fn cross_compartment_entry(&self) -> Result<()> {
        twizzler_abi::syscall::sys_thread_settls(0);
        if OUR_RUNTIME.is_monitor().is_some() {
            twizzler_abi::syscall::sys_thread_set_active_sctx_id(0.into()).inspect_err(|e| {
                twizzler_abi::klog_println!("failed to set sctx: {}", e);
            })?;
        } else {
            let _ = twizzler_abi::syscall::sys_sctx_attach(monitor_api::get_comp_config().sctx)
                .inspect_err(|e| {
                    if !matches!(e, TwzError::Naming(NamingError::AlreadyBound)) {
                        twizzler_abi::klog_println!("failed to attach sctx: {}", e);
                    }
                });
            twizzler_abi::syscall::sys_thread_set_active_sctx_id(
                monitor_api::get_comp_config().sctx,
            )
            .inspect_err(|e| {
                twizzler_abi::klog_println!("failed to set-a sctx: {}", e);
            })?;
        }
        let mut inner = THREAD_MGR.inner.lock();
        let id = inner.next_id().freeze();
        drop(inner);
        let tls = TLS_GEN_MGR
            .lock()
            .get_next_tls_info(None, || RuntimeThreadControl::new(id))
            .unwrap();
        twizzler_abi::syscall::sys_thread_settls(tls as u64);
        Ok(())
    }

    pub(super) fn impl_spawn(&self, args: twizzler_rt_abi::thread::ThreadSpawnArgs) -> Result<u32> {
        // Box this up so we can pass it to the new thread.
        let args = Box::new(args);
        let tls = TLS_GEN_MGR
            .lock()
            .get_next_tls_info(None, || RuntimeThreadControl::new(0))
            .unwrap();
        let stack_raw = unsafe {
            OUR_RUNTIME
                .alloc_zeroed(Layout::from_size_align(args.stack_size, MIN_STACK_ALIGN).unwrap())
        } as usize;

        // Take the thread management lock, so that when the new thread starts we cannot observe
        // that thread running without the management data being recorded.
        let mut inner = THREAD_MGR.inner.lock();
        let id = inner.next_id();

        // Set the thread's ID. After this the TCB is ready.
        unsafe {
            tls.as_mut().unwrap().runtime_data.set_id(id.id);
        }

        let stack_size = args.stack_size;
        let arg_raw = Box::into_raw(args) as usize;

        tracing::debug!(
            "spawning thread {} with stack {:x}, entry {:x}, and TLS {:p}",
            id.id,
            stack_raw,
            trampoline as usize,
            tls,
        );

        let new_args = ThreadSpawnArgs {
            stack_size,
            start: trampoline as usize,
            arg: arg_raw,
        };

        let thid: ObjID = {
            let res: Result<_> =
                monitor_api::monitor_rt_spawn_thread(new_args, tls as usize, stack_raw);

            match res {
                Ok(id) => ObjID::from(id),
                Err(e) => return Err(e),
            }
        };

        let thread_repr_obj = self.map_object(thid, MapFlags::READ | MapFlags::WRITE)?;

        let thread = InternalThread::new(
            thread_repr_obj,
            stack_raw,
            stack_size,
            arg_raw,
            id.freeze(),
            tls,
        );
        let id = thread.id;
        inner.all_threads.insert(thread.id, thread);

        Ok(id)
    }

    pub(super) fn impl_join(&self, id: u32, timeout: Option<std::time::Duration>) -> Result<()> {
        trace!("joining on thread {} with timeout {:?}", id, timeout);
        let repr = {
            let mut inner = THREAD_MGR.inner.lock();
            inner.scan_for_exited_except(id);
            inner
                .all_threads
                .get(&id)
                .ok_or(TwzError::Argument(ArgumentError::BadHandle))?
                .repr_handle()
                .clone()
        };
        let base =
            unsafe { (repr.start().add(NULLPAGE_SIZE) as *const ThreadRepr).as_ref() }.unwrap();
        loop {
            let (state, _code) = base
                .wait_until(ExecutionState::Exited, timeout)
                .ok_or(TwzError::TIMED_OUT)?;
            if state == ExecutionState::Exited {
                let mut inner = THREAD_MGR.inner.lock();
                inner.prep_cleanup(id);
                inner.do_thread_gc();
                trace!("join {} completed", id);
                return Ok(());
            }
        }
    }
}
