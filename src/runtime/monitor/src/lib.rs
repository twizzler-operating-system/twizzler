use secgate::secure_gate;

#[no_mangle]
pub fn monitor_main() {
    println!("Hello, world!");
}

#[secure_gate]
pub fn stream_writer_write() {}
