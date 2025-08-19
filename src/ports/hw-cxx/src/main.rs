fn main() {
    println!("Hello, world!");
    unsafe {
        testcxx();
    }
}

#[link(name = "cxxtest")]
unsafe extern "C" {
    fn testcxx() -> i32;
}
