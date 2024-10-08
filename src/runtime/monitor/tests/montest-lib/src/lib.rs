#![feature(naked_functions)]
#![feature(thread_local)]

extern crate twz_rt;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[secgate::secure_gate]
pub fn test_thread_local_call_count() -> usize {
    #[thread_local]
    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    CALL_COUNT.fetch_add(1, Ordering::SeqCst) + 1
}

#[secgate::secure_gate]
pub fn test_global_call_count() -> usize {
    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    CALL_COUNT.fetch_add(1, Ordering::SeqCst) + 1
}

#[secgate::secure_gate]
pub fn test_internal_panic() -> usize {
    panic!("test panic")
}

#[secgate::secure_gate]
pub fn test_was_ctor_run() -> bool {
    WAS_CTOR_RUN.load(Ordering::SeqCst)
}

static WAS_CTOR_RUN: AtomicBool = AtomicBool::new(false);

#[used]
#[doc(hidden)]
#[allow(non_upper_case_globals)]
#[link_section = ".init_array"]
static ___cons_test___ctor: unsafe extern "C" fn() = {
    #[allow(non_snake_case)]
    #[link_section = ".text.startup"]
    unsafe extern "C" fn ___cons_test___ctor() {
        cons_test()
    }
    ___cons_test___ctor
};
unsafe fn cons_test() {
    WAS_CTOR_RUN.store(true, Ordering::SeqCst);
}
