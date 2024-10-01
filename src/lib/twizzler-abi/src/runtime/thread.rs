//! Implementation of the thread runtime.

use core::alloc::Layout;

use twizzler_runtime_api::{CoreRuntime, JoinError, SpawnError, ThreadRuntime};

use super::{object::InternalObject, MinimalRuntime};
use crate::{
    object::Protections,
    runtime::idcounter::IdCounter,
    rustc_alloc::collections::BTreeMap,
    simple_mutex::Mutex,
    syscall::{
        ThreadSpawnError, ThreadSpawnFlags, ThreadSync, ThreadSyncError, ThreadSyncFlags,
        ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
    },
    thread::{ExecutionState, ThreadRepr},
};

struct InternalThread {
    repr: InternalObject<ThreadRepr>,
    #[allow(dead_code)]
    int_id: u32,
}

use rustc_alloc::sync::Arc;
static THREAD_SLOTS: Mutex<BTreeMap<u32, Arc<InternalThread>>> = Mutex::new(BTreeMap::new());
static THREAD_ID_COUNTER: IdCounter = IdCounter::new_one();

fn get_thread_slots() -> &'static Mutex<BTreeMap<u32, Arc<InternalThread>>> {
    &THREAD_SLOTS
}

impl ThreadRuntime for MinimalRuntime {
    fn available_parallelism(&self) -> core::num::NonZeroUsize {
        crate::syscall::sys_info().cpu_count()
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

        let r = crate::syscall::sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                crate::syscall::ThreadSyncReference::Virtual32(futex),
                expected as u64,
                crate::syscall::ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
            timeout,
        );

        match r {
            Err(ThreadSyncError::Timeout) => return false,
            _ => return true,
        }
    }

    fn futex_wake(&self, futex: &core::sync::atomic::AtomicU32) -> bool {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            1,
        ));
        let _ = crate::syscall::sys_thread_sync(&mut [wake], None);
        // TODO
        false
    }

    fn futex_wake_all(&self, futex: &core::sync::atomic::AtomicU32) {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            usize::MAX,
        ));
        let _ = crate::syscall::sys_thread_sync(&mut [wake], None);
    }

    #[allow(dead_code)]
    #[allow(unused_variables)]
    #[allow(unreachable_code)]
    fn spawn(&self, args: twizzler_runtime_api::ThreadSpawnArgs) -> Result<u32, SpawnError> {
        const STACK_ALIGN: usize = 32;
        let stack_layout = Layout::from_size_align(args.stack_size, STACK_ALIGN).unwrap();
        if args.stack_size == 0 {
            return Err(SpawnError::InvalidArgument);
        }
        let stack_base = unsafe { self.default_allocator().alloc(stack_layout) };
        let (tls_set, tls_base, tls_len, tls_align) =
            crate::runtime::tls::new_thread_tls().unwrap();
        let tls_layout = Layout::from_size_align(tls_len, tls_align).unwrap();
        let initial_stack = stack_base as usize;
        let initial_tls = tls_set;
        let thid = unsafe {
            crate::syscall::sys_spawn(crate::syscall::ThreadSpawnArgs {
                entry: args.start,
                stack_base: initial_stack,
                stack_size: args.stack_size,
                tls: initial_tls,
                arg: args.arg,
                flags: ThreadSpawnFlags::empty(),
                vm_context_handle: None,
                upcall_target: crate::syscall::UpcallTargetSpawnOption::Inherit,
            })?
        };

        let int_id = THREAD_ID_COUNTER.fresh();
        let thread = InternalThread {
            repr: InternalObject::map(thid, Protections::READ).unwrap(),
            int_id,
        };
        get_thread_slots().lock().insert(int_id, Arc::new(thread));
        Ok(int_id)
    }

    fn yield_now(&self) {
        crate::syscall::sys_thread_yield()
    }

    fn set_name(&self, _name: &core::ffi::CStr) {}

    fn sleep(&self, duration: core::time::Duration) {
        let _ = crate::syscall::sys_thread_sync(&mut [], Some(duration));
    }

    fn join(&self, id: u32, timeout: Option<core::time::Duration>) -> Result<(), JoinError> {
        loop {
            let thread = {
                get_thread_slots()
                    .lock()
                    .get(&id)
                    .cloned()
                    .ok_or(JoinError::LookupError)?
            };
            let data = thread.repr.base().wait(timeout);
            if let Some(data) = data {
                if data.0 == ExecutionState::Exited {
                    get_thread_slots().lock().remove(&id);
                    THREAD_ID_COUNTER.release(id);
                    return Ok(());
                }
            } else if timeout.is_some() {
                return Err(JoinError::Timeout);
            }
        }
    }

    fn tls_get_addr(&self, _tls_index: &twizzler_runtime_api::TlsIndex) -> Option<*const u8> {
        panic!("minimal runtime only supports LocalExec TLS model");
    }
}

impl From<ThreadSpawnError> for SpawnError {
    fn from(ts: ThreadSpawnError) -> Self {
        match ts {
            ThreadSpawnError::Unknown => Self::Other,
            ThreadSpawnError::InvalidArgument => Self::InvalidArgument,
            ThreadSpawnError::NotFound => Self::ObjectNotFound,
        }
    }
}
