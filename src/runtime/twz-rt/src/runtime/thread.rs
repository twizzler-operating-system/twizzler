//! Implements thread management routines. Largely unimplemented still.

use std::{
    collections::HashMap,
    ffi::CString,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
};

use dynlink::tls::{Tcb, TlsRegion};
use twizzler_abi::syscall::{
    sys_thread_sync, sys_thread_yield, ThreadSync, ThreadSyncError, ThreadSyncFlags, ThreadSyncOp,
    ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_runtime_api::ThreadRuntime;

use crate::preinit_println;

use super::ReferenceRuntime;

const THREAD_NAME_MAX: usize = 128;
pub struct RuntimeThreadControl {
    internal_lock: AtomicU32,
    id: u32,
    inner: std::cell::UnsafeCell<RuntimeThreadControlInner>,
}

pub struct RuntimeThreadControlInner {
    name: [u8; THREAD_NAME_MAX + 1],
}

impl RuntimeThreadControl {
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
    id: u32,
    tls: TlsRegion,
}

struct ThreadManager {
    inner: Mutex<ThreadManagerInner>,
}

struct ThreadManagerInner {
    all_threads: HashMap<u32, InternalThread>,
}

fn with_current_thread<R, F: FnOnce(&RuntimeThreadControl) -> R>(f: F) -> R {
    let tp: &mut Tcb<RuntimeThreadControl> = unsafe {
        dynlink::tls::get_current_thread_control_block()
            .as_mut()
            .unwrap()
    };
    f(&tp.runtime_data)
}

// TODO: implement spawning and joining

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
        _args: twizzler_runtime_api::ThreadSpawnArgs,
    ) -> Result<u32, twizzler_runtime_api::SpawnError> {
        todo!()
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

    fn join(
        &self,
        _id: u32,
        _timeout: Option<std::time::Duration>,
    ) -> Result<(), twizzler_runtime_api::JoinError> {
        preinit_println!("HERE: join");
        todo!()
    }

    fn tls_get_addr(&self, index: &twizzler_runtime_api::TlsIndex) -> Option<*const u8> {
        let tp: &Tcb<()> = unsafe {
            dynlink::tls::get_current_thread_control_block()
                .as_ref()
                .expect("failed to find thread control block")
        };
        tp.get_addr(index)
    }
}
