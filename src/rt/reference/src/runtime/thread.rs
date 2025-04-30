//! Implements thread management routines.

use dynlink::tls::Tcb;
use twizzler_abi::syscall::{
    sys_thread_sync, sys_thread_yield, ThreadSync, ThreadSyncFlags, ThreadSyncOp,
    ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_rt_abi::{
    error::TwzError,
    thread::{ThreadSpawnArgs, TlsIndex},
    Result,
};

use self::tcb::with_current_thread;
use super::ReferenceRuntime;
use crate::{preinit_println, runtime::thread::mgr::ThreadManager};

mod internal;
mod mgr;
mod tcb;

pub(crate) use tcb::TLS_GEN_MGR;

const MIN_STACK_ALIGN: usize = 128;

static THREAD_MGR: ThreadManager = ThreadManager::new();

impl ReferenceRuntime {
    pub fn available_parallelism(&self) -> core::num::NonZeroUsize {
        twizzler_abi::syscall::sys_info().cpu_count()
    }

    pub fn futex_wait(
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

        !matches!(r, Err(TwzError::TIMED_OUT))
    }

    pub fn futex_wake(&self, futex: &core::sync::atomic::AtomicU32, count: usize) -> bool {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            count,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
        false
    }

    pub fn yield_now(&self) {
        sys_thread_yield()
    }

    pub fn set_name(&self, name: &std::ffi::CStr) {
        with_current_thread(|cur| {
            THREAD_MGR.with_internal(cur.id(), |th| th.set_name(name));
        })
    }

    pub fn sleep(&self, duration: std::time::Duration) {
        let _ = sys_thread_sync(&mut [], Some(duration));
    }

    pub fn tls_get_addr(&self, index: &TlsIndex) -> Option<*mut u8> {
        let tp: &Tcb<()> = unsafe {
            match dynlink::tls::get_current_thread_control_block().as_ref() {
                Some(tp) => tp,
                None => {
                    preinit_println!("failed to locate TLS data");
                    self.abort();
                }
            }
        };

        tp.get_addr(index)
    }

    pub fn spawn(&self, args: ThreadSpawnArgs) -> Result<u32> {
        self.impl_spawn(args)
    }

    pub fn join(&self, id: u32, timeout: Option<std::time::Duration>) -> Result<()> {
        self.impl_join(id, timeout)
    }
}
