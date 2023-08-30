use fute::file::{File};
use std::str;

use std::io::{Read};
fn main() {
    let path = std::env::args().nth(3).expect("Path pls");

    let mut f = File::open(&path).expect("Couldn't open file :(");
    let buf: &mut [u8; 100] = &mut [0; 100];

    loop {
        let x = f.read(buf).expect("Can't read file :(");
        if x == 0 {break}

        let y = &buf[0..x];
        println!("{}",  str::from_utf8(y).unwrap());
    }
}
