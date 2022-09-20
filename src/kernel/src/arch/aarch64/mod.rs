mod start;

pub fn kernel_main() -> ! {
    crate::machine::serial::print_str(b"[kernel] hello world!!");
    loop {}
}