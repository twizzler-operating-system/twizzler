use std::io::Write;

use crypter::Crypter;
use fute::file::File;
use lethe_cli::fs::LetheFute;

use lethe_cli::crypt::Oreo;

use std::str;
fn main() {
    let mut send = "This is 40 bytes no doubt about it, cool".as_bytes();
    let mut send = "surprisingly this isn't 32 bytes".as_bytes();
    let mut send = "readers see this is 33 characters".as_bytes();

    let y = Oreo::onetime_encrypt(&[0u8; 32], send).unwrap();

    println!("{:?}", str::from_utf8(&Oreo::onetime_decrypt(&[0u8; 32], &y).unwrap()));



    let mut x = LetheFute::new();

    x.create("/hello.world");

    let mut send = "Important Military Secrets".as_bytes();
    println!("{}", x.write("/hello.world",   &mut send).unwrap());

    let receive: &mut [u8] = &mut [0; 32];
    println!("{}", x.read("/hello.world", receive).unwrap());

    let mut x = File::create("/a").unwrap();
    
    x.write(receive);
}