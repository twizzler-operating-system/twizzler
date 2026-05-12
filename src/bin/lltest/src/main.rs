fn main() {
    let lib = unsafe { libloading::Library::new("libllt.so").unwrap() };
    unsafe {
        let add_one: libloading::Symbol<unsafe extern "C" fn(u32) -> u32> =
            lib.get(b"add_one").unwrap();
        let result = add_one(1);
        println!("Result: {}", result);
    }
}
