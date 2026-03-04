#![feature(thread_local)]
#![feature(linkage)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use twizzler_rt_abi::Result;

#[secgate::entry(lib = "montest-lib")]
pub fn test_thread_local_call_count() -> Result<usize> {
    #[thread_local]
    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    Ok(CALL_COUNT.fetch_add(1, Ordering::SeqCst) + 1)
}

#[secgate::entry(lib = "montest-lib")]
pub fn test_global_call_count() -> Result<usize> {
    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    Ok(CALL_COUNT.fetch_add(1, Ordering::SeqCst) + 1)
}

#[secgate::entry(lib = "montest-lib")]
pub fn test_internal_panic(catch_it: bool) -> Result<usize> {
    if catch_it {
        let x = std::panic::catch_unwind(|| {
            panic!("test_panic (to be caught)");
        });
        return Ok(if x.is_err() { 1 } else { 0 });
    }
    panic!("test_panic (not caught)");
}

#[secgate::entry(lib = "montest-lib")]
pub fn test_was_ctor_run() -> Result<bool> {
    Ok(WAS_CTOR_RUN.load(Ordering::SeqCst))
}

#[secgate::entry(lib = "montest-lib")]
pub fn dynamic_test(x: u32) -> Result<u32> {
    Ok(42 + x)
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
