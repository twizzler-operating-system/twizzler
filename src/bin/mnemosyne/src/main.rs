extern crate twizzler_abi;

use std::{
    fs::DirBuilder,
    future,
    mem::size_of,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use layout::{io::SeekFrom, Read, Seek, Write};
use lethe_gadget_fat::filesystem::FileSystem;
use twizzler_async::{block_on, Task, Timer};

use crate::nvme::{init_nvme, NvmeController};
mod disk;
mod nvme;

use disk::Disk;

pub fn main() {
    let command = std::env::args().nth(1).unwrap();
    let id = std::env::args().nth(2).unwrap().parse::<u128>().unwrap();
    let value = std::env::args().nth(3).unwrap();

    let mut d = Disk::new().unwrap();

    match command.to_string().as_str() {
        "setup" => {
            disk::setup(&mut d);
            println!("Done with setup!");
        }
        "create" => {
            let mut fs = FileSystem::<Disk>::open(d);

            fs.create_object(id, 512).unwrap();
            fs.write_all(id, value.as_bytes(), 0).unwrap();
            println!("Done written {}!", value);
        }
        "read" => {
            let mut fs = FileSystem::<Disk>::open(d);

            let size = value.parse::<usize>().unwrap();
            let mut buf = Vec::<u8>::with_capacity(size);
            for i in 0..size {
                buf.push(0);
            }
            fs.read_exact(id, &mut buf, 0).unwrap();
            println!("str: {:?}", &buf);
            println!("str: {:?}", String::from_utf8_lossy(&buf));
        }
        _ => panic!("Command is invalid"),
    }
}
