//! Rountines and definitions for the thread control block.
//!
//! Note that the control struct here uses a manual lock instead of a Mutex.
//! This is because the thread-control block may be accessed by libstd (or any
//! library, really, nearly arbitrarily, so we just avoid any complex code in here
//! that might call into std (with one exception, below).

use std::{alloc::GlobalAlloc, collections::BTreeMap, panic::catch_unwind, sync::atomic::Ordering};

use dynlink::tls::Tcb;
use monitor_api::{RuntimeThreadControl, TlsTemplateInfo, THREAD_STARTED};
use twizzler_abi::simple_mutex::Mutex;

use crate::runtime::OUR_RUNTIME;

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
        });
        // Find the arguments. arg is a pointer to a Box::into_raw of a Box of ThreadSpawnArgs.
        let arg = unsafe {
            (arg as *const twizzler_rt_abi::thread::ThreadSpawnArgs)
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

#[derive(Default)]
pub(crate) struct TlsGenMgr {
    map: BTreeMap<u64, TlsGen>,
}

pub(crate) struct TlsGen {
    template: TlsTemplateInfo,
    thread_count: usize,
}

unsafe impl Send for TlsGen {}

pub(crate) static TLS_GEN_MGR: Mutex<TlsGenMgr> = Mutex::new(TlsGenMgr {
    map: BTreeMap::new(),
});

impl TlsGenMgr {
    pub fn _need_new_gen(&self, mygen: Option<u64>) -> bool {
        let cc = monitor_api::get_comp_config();
        let template = unsafe { cc.get_tls_template().as_ref().unwrap() };
        mygen.is_some_and(|mygen| mygen == template.gen)
    }

    pub fn get_next_tls_info<T>(
        &mut self,
        mygen: Option<u64>,
        new_tcb_data: impl FnOnce() -> T,
    ) -> Option<*mut Tcb<T>> {
        let cc = monitor_api::get_comp_config();
        let template = unsafe { cc.get_tls_template().as_ref().unwrap() };
        if mygen.is_some_and(|mygen| mygen == template.gen) {
            return None;
        }

        let new = unsafe { OUR_RUNTIME.get_alloc().alloc(template.layout) };
        let tlsgen = self.map.entry(template.gen).or_insert_with(|| TlsGen {
            template: *template,
            thread_count: 0,
        });
        tlsgen.thread_count += 1;

        unsafe {
            let tcb = tlsgen.template.init_new_tls_region(new, new_tcb_data());
            Some(tcb)
        }
    }

    // TODO: when threads exit or move on to a different TLS gen, track that in thread_count, and if
    // it hits zero, notify the monitor.
}
