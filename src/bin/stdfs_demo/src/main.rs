extern crate twizzler_abi;

use std::io::{Read};
use std::{fs::{File, OpenOptions}, io::Write};
use twizzler_abi::syscall::{ObjectCreate, BackingType, LifetimeType, ObjectCreateFlags};

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
        f.write("Hello world!\n".as_bytes());
    }
    
    {
        let mut f = File::create(id.as_u128().to_string()).unwrap();
        let mut buf: [u8; 13] = [0; 13];
        f.read(&mut buf);
        println!("{}", String::from_utf8(buf.to_vec()).unwrap());
    }

}
