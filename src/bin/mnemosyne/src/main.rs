extern crate twizzler_abi;

use std::{
    future,
    mem::size_of,
    sync::{Arc, Mutex, RwLock},
};
use twizzler_async::block_on;
use crate::nvme::{init_nvme, NvmeController};
use twizzler_async::Task;
use twizzler_async::Timer;
use std::time::Duration;

use lethe_gadget_fat::filesystem::FileSystem;

use layout::{Seek, Read, Write, io::SeekFrom};
mod disk;
mod nvme;

use disk::Disk;

pub fn main() {
    let command = std::env::args().nth(1).unwrap();
    let id = std::env::args().nth(2).unwrap().parse::<u128>().unwrap();
    let value = std::env::args().nth(3).unwrap();

    let mut d = Disk::new().unwrap();
    //disk::setup(&mut d);

    let mut fs = FileSystem::<Disk>::open(d);

    match command.to_string().as_str() {
        "create" => {
            fs.create_object(id, 512).unwrap();
            fs.write_all(id, value.as_bytes(), 0).unwrap();
            println!("Done written {}!", value);
        }
        "read" => {
            let size = value.parse::<usize>().unwrap();
            let mut buf = Vec::<u8>::with_capacity(size);
            for i in 0..size {
                buf.push(0);
            }
            fs.read_exact(id, &mut buf, 0).unwrap();
            println!("str: {:?}", &buf);
            println!("str: {:?}", String::from_utf8_lossy(&buf));
        }
        _ => panic!("Command is invalid")
    }

    /*let str = "Top military secrets";
    fs.create_object(id, 0x20).unwrap();

    fs.write_all(id, str.as_bytes(), 0).unwrap();

    let mut x: [u8; 20] = [0; 20];
    fs.read_exact(id, &mut x, 0).unwrap();*/

}
