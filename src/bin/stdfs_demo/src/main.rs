extern crate twizzler_abi;

use std::io::Read;
use std::fs::File;

fn main() {
    let id: u128 = 0x1000000000000000a;
    let mut f = File::create(id.to_string()).unwrap();

    let mut buf: [u8; 4096] = [0; 4096];
    println!("bytes read: {}", f.read(&mut buf).unwrap());

    println!("Status: {}", std::str::from_utf8(&buf).unwrap());
}
