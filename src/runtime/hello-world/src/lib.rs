#![feature(naked_functions)]
#![feature(thread_local)]

secgate::secgate_prelude!();

#[no_mangle]
pub extern "C" fn test_sec_call() {
    println!("trying sec call");
    println!(
        "got {:?} from sec call",
        monitor::secgate_test::bar(1, true)
    );
}

#[secgate::secure_gate]
pub fn bar(x: u32, y: bool) -> u32 {
    420
}
