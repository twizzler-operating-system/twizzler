extern crate twizzler_abi;

use std::io::{Read, Seek};
use std::{fs::{File, OpenOptions}, io::Write};
use twizzler_abi::syscall::{ObjectCreate, BackingType, LifetimeType, ObjectCreateFlags};

const SIZE: u64 = 1 << 20;
const OFFSET: u64 = 1 << 30;
fn main() {
    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    let id = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();

    println!("Created object {}.", id);
    
    {
        let mut f = File::create(id.as_u128().to_string()).unwrap();
        let buf: [u8; 4096] = [128; 4096];
        for i in 0..(SIZE/4096) {
            let x = f.write(&buf);
            println!("Wrote {} {}", i, x.unwrap());
        }
    }
    let mut b = false;

    {
        let mut f = File::create(id.as_u128().to_string()).unwrap();
        
        for j in 0..(SIZE/8192) {
            let mut buf: [u8; 8192] = [0; 8192];

            f.read(&mut buf);

            for i in 0..8192 {
                if buf[i] != 128 {
                    b = true;
                    break;
                }
            }
            println!("{:?}", buf);
            if b {
                break;
            }

            println!("Read {}", j);
        }
    }

    println!("Status: {}", b);
    
}
