#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(linkage)]

use std::sync::atomic::{AtomicBool, Ordering};

use twizzler_rt_abi::Result;

#[secgate::gatecall]
pub fn test_thread_local_call_count() -> Result<usize> {}

#[secgate::gatecall]
pub fn test_global_call_count() -> Result<usize> {}

#[secgate::gatecall]
pub fn test_internal_panic(catch_it: bool) -> Result<usize> {}

#[secgate::gatecall]
pub fn test_was_ctor_run() -> Result<bool> {}

#[secgate::gatecall]
pub fn dynamic_test(x: u32) -> Result<u32> {}

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
