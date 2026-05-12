use std::sync::atomic::{AtomicBool, Ordering};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn add_one(x: u32) -> u32 {
    println!(
        "add one called, and constructors were run? {}",
        WAS_CTOR_RUN.load(Ordering::SeqCst)
    );
    x + 1
}

static WAS_CTOR_RUN: AtomicBool = AtomicBool::new(false);

#[used]
#[doc(hidden)]
#[allow(non_upper_case_globals)]
#[unsafe(link_section = ".init_array")]
static ___cons_test___ctor: unsafe extern "C" fn() = {
    #[allow(non_snake_case)]
    #[unsafe(link_section = ".text.startup")]
    unsafe extern "C" fn ___cons_test___ctor() {
        unsafe { cons_test() }
    }
    ___cons_test___ctor
};
unsafe fn cons_test() {
    WAS_CTOR_RUN.store(true, Ordering::SeqCst);
}
