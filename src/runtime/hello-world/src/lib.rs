#![feature(thread_local)]
#[no_mangle]
pub extern "C" fn test_sec_call() {
    println!("trying sec call");
    let r = monitor::secgate_test::foo();
    test_tls();
    /*
    unsafe {
        not_a_real_symbol();
        another_symbol_that_doesnt_exist();
    }*/
    println!("got {:?} from sec call", r);
}

/*
extern "C" {
    fn not_a_real_symbol();
    fn another_symbol_that_doesnt_exist();
}
*/

#[thread_local]
static mut FOO: usize = 12;

pub fn test_tls() {
    unsafe {
        FOO += 1;
        println!("==> {}", FOO);
    }
}
