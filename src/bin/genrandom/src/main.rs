#![feature(new_uninit)]

extern crate twizzler_runtime;

use std::{
    fs::DirBuilder,
    future,
    mem::{size_of, zeroed, MaybeUninit},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, channel},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use getrandom::{getrandom, getrandom_uninit};
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
    const BUF_SIZE: u64 = 0x1000000;
    const BUF_SIZE_USIZE: usize = BUF_SIZE as usize;
    const START_OFFSET: u64 = 0;
    const OFFSET_ITER: u64 = START_OFFSET / BUF_SIZE;
    const ITER_CT: u64 = OBJ_SIZE / BUF_SIZE;
    // fs.create_object(id, 1500);
    d.seek(SeekFrom::Current(START_OFFSET as i64)).unwrap();
    println!("seeked forward");
    let mut buf = vec![0u8; BUF_SIZE_USIZE];
    let (tx, rx) = mpsc::sync_channel(1);
    let gen_thread = thread::spawn(move || {
        for i in OFFSET_ITER..ITER_CT {
            let mut buf = Box::new_uninit_slice(BUF_SIZE_USIZE);
            let start = Instant::now();
            getrandom_uninit(&mut (*buf)).unwrap();
            let out = unsafe { buf.assume_init() };
            print!(
                "Genrated {} / {} bytes in {:.2?},\t",
                i * BUF_SIZE,
                OBJ_SIZE,
                Instant::now() - start
            );
            tx.send(out).expect("should send message");
        }
    });
    let program_start = Instant::now();
    let write_thread = thread::spawn(move || {
        let mut iter_ct = OFFSET_ITER;
        for buf in rx {
            let start = Instant::now();
            d.write(&buf);
            let end = Instant::now();
            let curr_dur = end - program_start;
            let iters_passed = iter_ct - OFFSET_ITER + 1;
            let iters_left = ITER_CT - iter_ct;
            let time_left = (curr_dur / iters_passed as u32) * iters_left as u32;
            println!(
                "Wrote {} / {} bytes in {:.2?}, {:.2} hours left",
                iter_ct * BUF_SIZE,
                OBJ_SIZE,
                end - start,
                time_left.as_secs_f64() / 60.0 / 60.0
            );
            iter_ct += 1;
        }
    });
    // for i in OFFSET_ITER..ITER_CT {
    //     let start = Instant::now();
    //     getrandom(&mut buf);
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
