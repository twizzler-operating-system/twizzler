#![feature(thread_local)]
use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, Ordering},
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn add_one(x: u32) -> u32 {
    println!(
        "add one called, and constructors were run? {}",
        WAS_CTOR_RUN.load(Ordering::SeqCst)
    );
    x + 1
}

// TLS test surface: a tdata variable (nonzero initializer) and a tbss array, accessed through
// exported functions so lltest can drive them from threads that predate and postdate the dlopen.
#[thread_local]
static TLS_COUNTER: Cell<u64> = Cell::new(1000);

#[thread_local]
static TLS_BSS: [Cell<u64>; 32] = [const { Cell::new(0) }; 32];

#[unsafe(no_mangle)]
pub unsafe extern "C" fn tls_bump(n: u64) -> u64 {
    TLS_COUNTER.set(TLS_COUNTER.get() + n);
    TLS_COUNTER.get()
}

/// Returns the old value at `idx`, then stores `val` there.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tls_bss_swap(idx: usize, val: u64) -> u64 {
    let old = TLS_BSS[idx].get();
    TLS_BSS[idx].set(val);
    old
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
