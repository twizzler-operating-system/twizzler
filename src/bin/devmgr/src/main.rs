static FOO: u64 = 0;
#[used]
static mut BAR: [u8; 0x2111] = [0; 0x2111];

fn main() {
    unsafe {
        println!("Hello, world! {} {}", FOO, BAR.len());
    }

    let device_root = twizzler_driver::device::get_bustree_root();
    for device in device_root.children() {
        println!("{}", device);
    }
}
