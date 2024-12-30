fn main() {
    println!("Hello, world!");
    let mut lh = logboi::LogHandle::new().unwrap();
    lh.log(b"Logging Test!\n");
}