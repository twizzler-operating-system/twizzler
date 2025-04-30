fn main() {
    println!("Hello, world, from Rust!");
    unsafe { c_hello_world() };
}

#[link(name = "hw")]
unsafe extern "C" {
    fn c_hello_world() -> std::ffi::c_int;
}
