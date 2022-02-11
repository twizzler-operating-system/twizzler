static FOO: u64 = 0;
#[used]
static mut BAR: [u8; 0x2111] = [0; 0x2111];

fn main() {
    unsafe {
        println!("Hello, world! {} {}", FOO, BAR.len());
        for i in &BAR {
            println!("{}", i);
        }
    }
}
