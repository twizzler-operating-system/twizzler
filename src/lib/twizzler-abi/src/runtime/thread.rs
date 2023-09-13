use core::cell::OnceCell;

use slotmap::SlotMap;
use twizzler_runtime_api::{InternalError, ThreadRuntime};

use crate::{
    object::{InternalObject, Protections},
    simple_mutex::Mutex,
    syscall::{ThreadSpawnError, ThreadSpawnFlags, ThreadSync, ThreadSyncFlags, ThreadSyncSleep},
    thread::ThreadRepr,
};

use super::MinimalRuntime;

struct InternalThread {
    repr: InternalObject<ThreadRepr>,
}

static THREAD_SLOTS: OnceCell<Mutex<SlotMap<u32, InternalThread>>> = OnceCell::new();

fn get_thread_slots() -> &'static Mutex<SlotMap<u32, InternalThread>> {
    THREAD_SLOTS.get_or_init(|| Mutex::new(SlotMap::new()))
}

impl ThreadRuntime for MinimalRuntime {
    type InternalId = u32;

    type SpawnError = ThreadSpawnError;

    // 2MB stack size
    const DEFAULT_MIN_STACK_SIZE: usize = (1 << 20) * 2;

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
        if futex.load(core::sync::Atomic::Ordering::Relaxed) != expected {
            return true;
        }

        crate::syscall::sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                crate::syscall::ThreadSyncReference::Virtual32(futex),
                expected,
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
    ) -> Result<Self::InternalId, Self::SpawnError> {
        let initial_stack = todo!();
        let initial_tls = todo!();
        let thid = unsafe {
            crate::syscall::sys_spawn(crate::syscall::ThreadSpawnArgs {
                entry: args.start,
                stack_base: initial_stack,
                stack_size: args.stack_size,
                tls: initial_tls,
                arg: args.arg,
                flags: ThreadSpawnFlags::empty(),
                vm_context_handle: None,
            })?
        };

        let thread = InternalThread {
            repr: InternalObject::map(thid, Protections::READ).unwrap(),
        };
        let id = get_thread_slots().lock().insert(thread);
        Ok(id)
    }

    fn yield_now(&self) {
        crate::syscall::sys_thread_yield()
    }

    fn set_name(&self, name: &core::ffi::CStr) {
        // TODO
    }

    fn sleep(&self, duration: core::time::Duration) {
        let _ = crate::syscall::sys_thread_sync(&mut [], Some(duration));
    }

    fn join(&self, id: Self::InternalId, timeout: Option<Duration>) {
        let thread: InternalThread = get_thread_slots().lock().get(id);
        thread.repr.base().wait(timeout);
        get_thread_slots().lock().remove(id);
    }
}

impl InternalError for ThreadSpawnError {}

#[no_mangle]
#[linkage = "extern_weak"]
pub fn __tls_get_addr() {
    todo!()
}
