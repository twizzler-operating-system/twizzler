extern crate twizzler_abi;

use getrandom::getrandom;
// TODO: instead of running these very basic tests,
// find a way to stream random bytes out of twizzler and onto my local
// computer so I can run dieharder tests locally on my machine.
// Alternatively find a way to compile dieharder, a C library, in twizzler.
// debian package source: https://salsa.debian.org/edd/dieharder
fn main() {
    let mut into = [0u8; 1024];
    loop {
        getrandom(&mut into);
        for byte in into {
            print!("{}", byte as char);
        }
    }
}
