#![feature(random)]
use std::{io::Error, random::random};

use object_store::*;

fn make_and_check_file(buf1: &mut [u8], buf2: &mut [u8]) -> (Vec<u8>, u128) {
    let id: u128 = random();

    create_object(id).unwrap();
    buf1.fill_with(|| random());
    write_all(id, buf1, 0).unwrap();
    read_exact(id, buf2, 0).unwrap();
    assert!(buf1 == buf2);
    (buf2.into(), id)
}

fn main() {
    let mut working_bufs = (vec![0; 600], vec![0; 600]);
    let out = (0..20).map(|i| {
        println!("{}", i);
        make_and_check_file(&mut working_bufs.0, &mut working_bufs.1)
    });
    for (value, id) in out {
        // make sure buf == read
        let mut buf = vec![0; 600];
        read_exact(id, &mut buf, 0).unwrap();
        assert!(value == buf);
        // unlink
        unlink_object(id).unwrap();
        // make sure object is unlinked
        let v = read_exact(id, &mut buf, 0).expect_err("should be error");
        assert!(v.kind() == std::io::ErrorKind::NotFound);
    }
}
