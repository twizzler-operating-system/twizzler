//! Rountines and definitions for the thread control block.
//!
//! Note that the control struct here uses a manual lock instead of a Mutex.
//! This is because the thread-control block may be accessed by libstd (or any
//! library, really, nearly arbitrarily, so we just avoid any complex code in here
//! that might call into std (with one exception, below).

use std::{
    cell::UnsafeCell,
    collections::HashMap,
    panic::catch_unwind,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use dynlink::tls::Tcb;
use tracing::trace;
use twizzler_runtime_api::CoreRuntime;

use crate::runtime::OUR_RUNTIME;

const THREAD_STARTED: u32 = 1;
pub struct RuntimeThreadControl {
    // Need to keep a lock for the ID, though we don't expect to use it much.
    internal_lock: AtomicU32,
    flags: AtomicU32,
    id: UnsafeCell<u32>,
}

impl Default for RuntimeThreadControl {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeThreadControl {
    pub const fn new() -> Self {
        Self {
            internal_lock: AtomicU32::new(0),
            flags: AtomicU32::new(0),
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
            // If this happens, something has gone very wrong.
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

/// Run a closure using the current thread's control struct as the argument.
pub(super) fn with_current_thread<R, F: FnOnce(&RuntimeThreadControl) -> R>(f: F) -> R {
    let tp: &mut Tcb<RuntimeThreadControl> = unsafe {
        dynlink::tls::get_current_thread_control_block()
            .as_mut()
            .unwrap()
    };
    f(&tp.runtime_data)
}

// Entry point for threads.
pub(super) extern "C" fn trampoline(arg: usize) -> ! {
    // This is the same code used by libstd on catching a panic and turning it into an exit code.
    const THREAD_PANIC_CODE: u64 = 101;
    let code = catch_unwind(|| {
        // Indicate that we are alive.
        with_current_thread(|cur| {
            // Needs an acq barrier here for the ID, but also a release for the flags.
            cur.flags.fetch_or(THREAD_STARTED, Ordering::SeqCst);
            trace!("thread {} started", cur.id());
        });
        // Find the arguments. arg is a pointer to a Box::into_raw of a Box of ThreadSpawnArgs.
        let arg = unsafe {
            (arg as *const twizzler_runtime_api::ThreadSpawnArgs)
                .as_ref()
                .unwrap()
        };
        // Jump to the requested entry point. Handle the return, just in case, but this is
        // not supposed to return.
        let entry: extern "C" fn(usize) = unsafe { core::mem::transmute(arg.start) };
        (entry)(arg.arg);
        0
    })
    .unwrap_or(THREAD_PANIC_CODE);
    twizzler_abi::syscall::sys_thread_exit(code);
}

pub struct TlsGen {
    pub gen_id: u64,
    pub thread_ptr: usize,
}

struct TlsGenCounter {
    pub tls_gen: TlsGen,
    pub count: AtomicU64,
}

#[derive(Default)]
pub struct TlsMgr {
    generations: HashMap<u64, TlsGenCounter>,
}

impl TlsMgr {
    fn add_new_gen(&mut self, gen: TlsGen) {
        self.generations.insert(
            gen.gen_id,
            TlsGenCounter {
                tls_gen: gen,
                count: AtomicU64::new(0),
            },
        );
    }

    fn increment_gen(&mut self, gen_id: u64) {
        self.generations
            .get_mut(&gen_id)
            .map(|gc| gc.count.fetch_add(1, Ordering::SeqCst));
    }

    fn decrement_gen(&mut self, gen_id: u64) -> bool {
        self.generations
            .get_mut(&gen_id)
            .map(|gc| gc.count.fetch_sub(1, Ordering::SeqCst) == 1)
            .unwrap_or(false)
    }
}

lazy_static::lazy_static! {
pub(crate) static ref TLS_MGR: TlsMgr = TlsMgr::default();
}
