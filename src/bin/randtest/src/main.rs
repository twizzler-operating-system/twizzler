#![feature(random)]
extern crate twizzler_runtime;

use std::random::random;

use getrandom::getrandom;

// TODO: instead of running these very basic tests,
// find a way to stream random bytes out of twizzler and onto my local
// computer so I can run dieharder tests locally on my machine.
// Alternatively find a way to compile dieharder, a C library, in twizzler.
// debian package source: https://salsa.debian.org/edd/dieharder
fn main() {
    let mut into1: [u8; 32] = Default::default();
    for b in into1.as_mut() {
        *b = random();
    }
    let mut into2: [u8; 32] = Default::default();
    getrandom(&mut into2).unwrap();

    println!("bytes: {:?}, {:?}", into1, into2);
}
