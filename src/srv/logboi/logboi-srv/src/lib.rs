#![feature(naked_functions)]
#![feature(linkage)]

use std::sync::atomic::AtomicUsize;

extern crate twizzler_runtime;

static NRCALL: AtomicUsize = AtomicUsize::new(0);

pub type Bar = u32;
#[secgate::secure_gate]
pub fn foo(bar: Bar) {
    println!(
        "FOO: {}, {}",
        bar,
        NRCALL.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
}
