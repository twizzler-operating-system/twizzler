//! Thread management routines, including spawn and join.

use std::{alloc::Layout, collections::HashMap, sync::Mutex};

use dynlink::tls::TlsRegion;
use tracing::trace;
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::sys_spawn,
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_runtime_api::{CoreRuntime, JoinError, MapFlags, ObjectRuntime, SpawnError};

use crate::{
    monitor::get_monitor_actions,
    runtime::{
        thread::{
            tcb::{trampoline, RuntimeThreadControl},
            MIN_STACK_ALIGN, THREAD_MGR,
        },
        ReferenceRuntime, OUR_RUNTIME,
    },
};

use super::internal::InternalThread;

pub(super) struct ThreadManager {
    inner: Mutex<ThreadManagerInner>,
}

impl ThreadManager {
    pub(super) fn new() -> Self {
        Self {
            inner: Mutex::new(ThreadManagerInner::new()),
        }
    }

    pub fn with_internal<R, F: FnOnce(&InternalThread) -> R>(&self, id: u32, f: F) -> Option<R> {
        let inner = self.inner.lock().unwrap();
        Some(f(inner.all_threads.get(&id)?))
    }
}

#[derive(Default)]
struct ThreadManagerInner {
    all_threads: HashMap<u32, InternalThread>,
    // Threads that have exited, but we haven't cleaned up yet.
    to_cleanup: Vec<InternalThread>,
    // Basic unique-ID system.
    id_stack: Vec<u32>,
    next_id: u32,
}

unsafe impl Send for ThreadManager {}
unsafe impl Sync for ThreadManager {}

impl ThreadManagerInner {
    fn new() -> Self {
        Self {
            next_id: 1,
            ..Default::default()
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
    pub(super) fn impl_spawn(
        &self,
        args: twizzler_runtime_api::ThreadSpawnArgs,
    ) -> Result<u32, twizzler_runtime_api::SpawnError> {
        // Box this up so we can pass it to the new thread.
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

        // Set the thread's ID. After this the TCB is ready.
        unsafe {
            tls.get_thread_control_block::<RuntimeThreadControl>()
                .as_mut()
                .unwrap()
                .runtime_data
                .set_id(id.id);
        }

        let stack_size = args.stack_size;
        let arg_raw = Box::into_raw(args) as usize;

        trace!(
            "spawning thread {} with stack {:x}, entry {:x}, and TLS {:x}",
            id.id,
            stack_raw,
            trampoline as usize,
            tls.get_thread_pointer_value(),
        );

        let thid = unsafe {
            sys_spawn(twizzler_abi::syscall::ThreadSpawnArgs {
                entry: trampoline as usize,
                stack_base: stack_raw,
                stack_size: stack_size,
                tls: tls.get_thread_pointer_value(),
                arg: arg_raw,
                flags: twizzler_abi::syscall::ThreadSpawnFlags::empty(),
                vm_context_handle: None,
            })
        }
        .map_err(|_| twizzler_runtime_api::SpawnError::KernelError)?;

        let thread_repr_obj = self
            .map_object(thid.as_u128(), MapFlags::READ | MapFlags::WRITE)
            .map_err(|_| SpawnError::Other)?;

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

    pub(super) fn impl_join(
        &self,
        id: u32,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), JoinError> {
        trace!("joining on thread {} with timeout {:?}", id, timeout);
        let repr = {
            let mut inner = THREAD_MGR.inner.lock().unwrap();
            inner.scan_for_exited_except(id);
            inner
                .all_threads
                .get(&id)
                .ok_or(JoinError::LookupError)?
                .repr_handle()
                .clone()
        };
        let base =
            unsafe { (repr.start.add(NULLPAGE_SIZE) as *const ThreadRepr).as_ref() }.unwrap();
        loop {
            let (state, _code) = base.wait(timeout).ok_or(JoinError::Timeout)?;
            if state == ExecutionState::Exited {
                let mut inner = THREAD_MGR.inner.lock().unwrap();
                inner.prep_cleanup(id);
                inner.do_thread_gc();
                trace!("join {} completed", id);
                return Ok(());
            }
        }
    }
}
