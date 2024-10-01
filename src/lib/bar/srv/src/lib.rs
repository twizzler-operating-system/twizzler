#![feature(naked_functions)]
#![feature(thread_local)]

extern crate twz_rt;

#[thread_local]
pub static mut TL: u32 = 0;

pub static mut GL: u32 = 0;

#[secgate::secure_gate]
pub fn bar_test() -> (u32, u32) {
    unsafe {
        TL += 1;
        GL += 1;
        (TL, GL)
    }
}

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
    println!("bar: constructor run!");
}
