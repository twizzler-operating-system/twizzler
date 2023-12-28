#[no_mangle]
pub extern "C" fn test_sec_call() {
    println!("trying sec call");
    let r = monitor::secgate_test::foo();
    println!("got {} from sec call", r);
}
