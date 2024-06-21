#![feature(naked_functions)]

#[secgate::secure_gate]
pub fn logboi_test(x: u32) -> u32 {
    println!("logboi got secure gate call ==> {}", x);
    x + 1
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
    println!("LBI: CONS TEST");
}
