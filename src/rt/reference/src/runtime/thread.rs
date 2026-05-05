//! Implements thread management routines.

use std::ffi::c_void;

use dynlink::tls::Tcb;
use twizzler_abi::syscall::{
    sys_thread_sync, sys_thread_yield, ThreadSync, ThreadSyncFlags, ThreadSyncOp,
    ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_rt_abi::{
    bindings::{thread_info, twz_error},
    thread::{ThreadSpawnArgs, TlsIndex},
    Result,
};

use super::ReferenceRuntime;
use crate::{
    preinit_println,
    runtime::thread::{internal::InternalThread, mgr::ThreadManager},
};

mod internal;
mod mgr;
mod tcb;
pub(crate) use tcb::{libc_init_tcb, with_current_thread, TLS_GEN_MGR};

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
    ) -> twz_error {
        // No need to wait if the value already changed.
        if futex.load(core::sync::atomic::Ordering::Relaxed) != expected {
            return 0;
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

        match r {
            Err(e) => return e.raw(),
            _ => return 0,
        }
    }

    pub fn futex_wake(
        &self,
        futex: &core::sync::atomic::AtomicU32,
        count: usize,
    ) -> twizzler_rt_abi::bindings::twz_error {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            count,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
        0
    }

    pub fn yield_now(&self) {
        sys_thread_yield()
    }

    pub fn set_name(&self, name: &std::ffi::CStr) {
        with_current_thread(|cur| {
            THREAD_MGR.with_internal(cur.id(), |th| th.set_name(name));
        })
    }

    pub fn get_name(&self, _tcb: *const c_void, name: &mut [u8]) -> usize {
        // TODO: if _tcb is non null and points to a different thread than our own, read that
        // thread's name.
        with_current_thread(|cur| {
            THREAD_MGR
                .with_internal(cur.id(), |th| th.get_name(name))
                .unwrap_or_else(|| {
                    name.fill(0);
                    0
                })
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

    pub fn spawn(&self, args: ThreadSpawnArgs) -> Result<(u32, *mut c_void)> {
        self.impl_spawn(args)
    }

    pub fn join(&self, id: u32, timeout: Option<std::time::Duration>) -> Result<()> {
        self.impl_join(id, timeout)
    }

    pub fn thread_get_info(&self, id: Option<u32>) -> thread_info {
        let make_info = |th: &InternalThread| -> thread_info {
            thread_info {
                id: th.id,
                tcb: th.tls.cast(),
                objid: th.objid().raw(),
            }
        };
        let id = id.unwrap_or_else(|| with_current_thread(|cur| cur.id()));
        THREAD_MGR
            .with_internal(id, make_info)
            .unwrap_or(thread_info {
                id,
                tcb: core::ptr::null_mut(),
                objid: 0,
            })
    }
}
