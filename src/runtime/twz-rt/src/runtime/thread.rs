//! Implements thread management routines.

use dynlink::tls::Tcb;
use lazy_static::lazy_static;
use tracing::trace;
use twizzler_abi::syscall::{
    sys_thread_sync, sys_thread_yield, ThreadSync, ThreadSyncError, ThreadSyncFlags, ThreadSyncOp,
    ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_runtime_api::{JoinError, SpawnError, ThreadRuntime, TlsIndex};

use crate::runtime::thread::mgr::ThreadManager;

use self::tcb::with_current_thread;

use super::ReferenceRuntime;

mod internal;
mod mgr;
mod tcb;

pub use tcb::RuntimeThreadControl;
pub(crate) use tcb::TLS_GEN_MGR;

const MIN_STACK_ALIGN: usize = 128;

lazy_static! {
    static ref THREAD_MGR: ThreadManager = ThreadManager::new();
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

    fn yield_now(&self) {
        sys_thread_yield()
    }

    fn set_name(&self, name: &std::ffi::CStr) {
        with_current_thread(|cur| {
            trace!("naming thread {} `{}'", cur.id(), name.to_string_lossy());
            THREAD_MGR.with_internal(cur.id(), |th| th.set_name(name));
        })
    }

    fn sleep(&self, duration: std::time::Duration) {
        let _ = sys_thread_sync(&mut [], Some(duration));
    }

    fn tls_get_addr(&self, index: &TlsIndex) -> Option<*const u8> {
        let tp: &Tcb<()> = unsafe {
            dynlink::tls::get_current_thread_control_block()
                .as_ref()
                .expect("failed to find thread control block")
        };
        tp.get_addr(index)
    }

    fn spawn(&self, args: twizzler_runtime_api::ThreadSpawnArgs) -> Result<u32, SpawnError> {
        self.impl_spawn(args)
    }

    fn join(&self, id: u32, timeout: Option<std::time::Duration>) -> Result<(), JoinError> {
        self.impl_join(id, timeout)
    }
}
