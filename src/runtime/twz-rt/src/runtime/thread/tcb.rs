use std::{
    cell::UnsafeCell,
    panic::catch_unwind,
    sync::atomic::{AtomicU32, Ordering},
};

use dynlink::tls::Tcb;
use tracing::trace;
use twizzler_runtime_api::CoreRuntime;

use crate::runtime::OUR_RUNTIME;

const THREAD_STARTED: u32 = 1;
pub struct RuntimeThreadControl {
    internal_lock: AtomicU32,
    flags: AtomicU32,
    id: UnsafeCell<u32>,
}

impl RuntimeThreadControl {
    pub fn new() -> Self {
        Self {
            internal_lock: AtomicU32::default(),
            flags: AtomicU32::default(),
            id: UnsafeCell::new(0),
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

    pub fn set_id(&self, id: u32) {
        self.write_lock();
        unsafe {
            *self.id.get().as_mut().unwrap() = id;
        }
        self.write_unlock();
    }

    pub fn id(&self) -> u32 {
        self.read_lock();
        let id = unsafe { *self.id.get().as_ref().unwrap() };
        self.read_unlock();
        id
    }
}

pub(super) fn with_current_thread<R, F: FnOnce(&RuntimeThreadControl) -> R>(f: F) -> R {
    let tp: &mut Tcb<RuntimeThreadControl> = unsafe {
        dynlink::tls::get_current_thread_control_block()
            .as_mut()
            .unwrap()
    };
    f(&tp.runtime_data)
}

pub(super) extern "C" fn trampoline(arg: usize) -> ! {
    let code = catch_unwind(|| {
        with_current_thread(|cur| {
            // Needs an acq barrier here for the ID, but also a release for the flags.
            cur.flags.fetch_or(THREAD_STARTED, Ordering::SeqCst);
            trace!("thread {} started", cur.id());
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
