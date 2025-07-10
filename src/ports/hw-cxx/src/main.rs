fn main() {
    println!("Hello, world!");
}

#[link("testcxx")]
extern "C" {
    fn testcxx() -> i32;
}
