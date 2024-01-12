#![feature(naked_functions)]
#![feature(thread_local)]

extern crate secgate;

#[no_mangle]
pub extern "C" fn test_sec_call() {
    println!("trying sec call");
    println!(
        "got {:?} from sec call",
        monitor::secgate_test::bar(1, true)
    );
    /*
    unsafe {
        not_a_real_symbol();
        another_symbol_that_doesnt_exist();
    }*/
    // println!("got {:?} from sec call", r);
}

/*
extern "C" {
    fn not_a_real_symbol();
    fn another_symbol_that_doesnt_exist();
}
*/

#[secgate::secure_gate]
pub fn bar(x: u32, y: bool) -> u32 {
    420
}

#[link(name = "calloca", kind = "static")]
extern "C" {
    pub fn c_with_alloca();
}
