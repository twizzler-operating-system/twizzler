#![feature(thread_local)]

extern crate monitor;
extern crate twz_rt;

fn main() {
    let mut logger = logboi::LogHandle::new().unwrap();
    logger.log("This is a logging test".as_bytes());

    let x = bar::bar_test().unwrap();
    println!("got: {}", x);
}
