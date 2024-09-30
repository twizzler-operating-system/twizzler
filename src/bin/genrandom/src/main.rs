extern crate twizzler_abi;

use std::{
    fs::DirBuilder,
    future,
    mem::{size_of, zeroed, MaybeUninit},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::channel,
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use getrandom::getrandom;
use layout::{io::SeekFrom, Read, Seek, Write};
use lethe_gadget_fat::filesystem::FileSystem;
use twizzler_async::{block_on, Task, Timer};

use crate::nvme::{init_nvme, NvmeController};
mod disk;
mod nvme;

use disk::Disk;

pub fn main() {
    let id = 20;
    println!("Running genrandom");

    let mut d = Disk::new().unwrap();
    disk::setup(&mut d);
    println!("Created disk");
    // // return;
    const OBJ_SIZE: u64 = 0x4_000_000_000;
    const BUF_SIZE: u64 = 0x1000;
    const START_OFFSET: u64 = 6341787648;
    const OFFSET_ITER: u64 = START_OFFSET / BUF_SIZE;
    const ITER_CT: u64 = OBJ_SIZE / BUF_SIZE;
    // fs.create_object(id, 1500);
    let (tx, rx) = channel();
    d.seek(SeekFrom::Start(START_OFFSET)).unwrap();
    println!("seeked forward");
    let program_start = Instant::now();
    let gen_thread = thread::spawn(move || {
        for i in OFFSET_ITER..ITER_CT {
            let buf = MaybeUninit<[u8; 1024]>::uninit();
            getrandom(&mut buf);
            let out = unsafe {buf.assume_init()};
            print!("Genrated bytes in {:?},\t", getrandom_time);
            tx.send(out).expect("should send message");
        }
    });
    let write_thread = thread::spawn(move || {
        for buf in rx {
            d.write(buf);
        }
    });
    // for i in OFFSET_ITER..ITER_CT {
    //     let start = Instant::now();
    //     let end = Instant::now();
    //     let getrandom_time = end - start;
    //     print!("Genrated bytes in {:?},\t", getrandom_time);
    //     let start = Instant::now();
    //     d.write(&buf).unwrap();
    //     let end = Instant::now();
    //     let write_time = end - start;
    //     let curr_dur = end - program_start;
    //     let iters_passed = i - OFFSET_ITER + 1;
    //     let iters_left = ITER_CT - i;
    //     let time_left = (curr_dur / iters_passed as u32) * iters_left as u32;

    //     println!(
    //         "wrote bytes {} / {} in {:?}; time left: {:.2}h",
    //         i * BUF_SIZE,
    //         OBJ_SIZE,
    //         write_time,
    //         time_left.as_secs_f64() / 60.0 / 60.0
    //     );
    // }
    println!("Done generating bytes");
    return;
    // match command.to_string().as_str() {
    //     "setup" => {
    //         disk::setup(&mut d);
    //         println!("Done with setup!");
    //     }
    //     "create" => {
    //         let mut fs = FileSystem::<Disk>::open(d);

    //         fs.create_object(id, 512).unwrap();
    //         fs.write_all(id, value.as_bytes(), 0).unwrap();
    //         println!("Done written {}!", value);
    //     }
    //     "read" => {
    //         let mut fs = FileSystem::<Disk>::open(d);

    //         let size = value.parse::<usize>().unwrap();
    //         let mut buf = Vec::<u8>::with_capacity(size);
    //         for i in 0..size {
    //             buf.push(0);
    //         }
    //         fs.read_exact(id, &mut buf, 0).unwrap();
    //         println!("str: {:?}", &buf);
    //         println!("str: {:?}", String::from_utf8_lossy(&buf));
    //     }
    //     _ => panic!("Command is invalid"),
    // }
}
