use std::{
    fs::File,
    io::{BufReader, Read},
};

use object_store::{FS, *};
use obliviate_core::kms::PersistableKeyManagementScheme;

fn get_unique_id() -> u128 {
    let mut id: u128 = rand::random();

    while !create_object(id).unwrap() {
        id = rand::random();
    }
    id
}

fn make_and_check_file(buf1: &mut [u8], buf2: &mut [u8]) -> (Vec<u8>, u128) {
    let id: u128 = get_unique_id();
    // let random_value = rand::random();
    // println!("{}", random_value);
    buf1.fill_with(|| rand::random());
    write_all(id, buf1, 0).unwrap();
    read_exact(id, buf2, 0).unwrap();
    assert!(buf1 == buf2);
    (buf2.into(), id)
}

pub fn zero_length_file() {
    let buf = vec![0u8; 5000];
    create_object(0).unwrap();
    write_all(0, &buf, 0).unwrap();
    unlink_object(0).unwrap();
}

fn _find_nonzero() {
    let file = File::open("test2.img").unwrap();
    let mut reader = BufReader::new(file);
    let mut byte: [u8; 1] = [0];
    let mut bytes: [u8; 8] = [0; 8];
    let mut index = 0;
    while let Ok(_) = reader.read_exact(&mut byte) {
        if byte[0] != 0 {
            print!("{}..", index);
            while let Ok(_) = reader.read_exact(&mut bytes) {
                if bytes == [0; 8] {
                    break;
                }
                index += 8;
            }
        }
        index += 1;
    }
}

fn get_all_ids() {
    let _all_ids = get_all_object_ids().unwrap();
}

fn test_lfn() {
    let id1: u128 = get_unique_id();
    let id2: u128 = id1 + 1;
    assert!(create_object(id2).unwrap());
    write_all(id1, b"asdf", 0).unwrap();
    write_all(id2, b"ghjk", 0).unwrap();

    let mut b1: [u8; 4] = [0; 4];
    let mut b2: [u8; 4] = [0; 4];
    read_exact(id1, &mut b1, 0).unwrap();
    read_exact(id2, &mut b2, 0).unwrap();
    assert!(&b1 == b"asdf");
    assert!(&b2 == b"ghjk");
}

fn test_khf_serde() {
    let id: u128 = get_unique_id();
    create_object(id).unwrap();
    write_all(id, b"asdf", 0).unwrap();
    advance_epoch().unwrap();
    const ROOT_KEY: [u8; 32] = [0; 32];
    let fs = FS.lock().unwrap();
    let mut khf = KHF.lock().unwrap();
    *khf = MyKhf::load(ROOT_KEY, "lethe/khf", &fs).unwrap();
    drop(khf);
    drop(fs);
    let mut buf = [0u8; 4];
    read_exact(id, &mut buf, 0).unwrap();
    assert!(&buf == b"asdf");
}

fn it_works() {
    let mut working_bufs = (vec![0; 5000], vec![0; 5000]);
    // println!("{:?}", KHF.lock().unwrap());
    let out = (0..5).map(|_i| make_and_check_file(&mut working_bufs.0, &mut working_bufs.1));
    advance_epoch().unwrap();
    *(KHF.lock().unwrap()) = MyKhf::load(ROOT_KEY, "lethe/khf", &FS.lock().unwrap()).unwrap();
    // println!("{:?}", KHF.lock().unwrap());
    for (value, id) in out {
        // make sure buf == read
        let mut buf = vec![0; 5000];
        read_exact(id, &mut buf, 0).unwrap();
        assert!(value == buf);
        // unlink
        unlink_object(id).unwrap();
        advance_epoch().unwrap();
        *(KHF.lock().unwrap()) = MyKhf::load(ROOT_KEY, "lethe/khf", &FS.lock().unwrap()).unwrap();
        // println!("{:?}", KHF.lock().unwrap());
        // make sure object is unlinked
        let v = read_exact(id, &mut buf, 0).expect_err("should be error");
        assert!(v.kind() == std::io::ErrorKind::NotFound);
    }
}

fn main() {
    print!("Test it_works...");
    it_works();
    println!("passed");
    print!("Test khf_serde...");
    test_khf_serde();
    println!("passed");
    print!("Test test_lfn...");
    test_lfn();
    println!("passed");
    print!("Test zero_length_file...");
    zero_length_file();
    println!("passed");
    print!("Test get_all_ids...");
    get_all_ids();
    println!("passed");
}
