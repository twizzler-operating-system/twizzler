use std::fs::{File, OpenOptions};

use layout::{collections::raw::RawBytes, io::StdIO, Encode, SourcedDynamic};
use lethe_gadget_fat::{
    filesystem::FileSystem,
    schema::{self, FATEntry, Superblock},
};

fn setup(data: &mut StdIO<File>) {
    let super_block = Superblock {
        magic: 0,
        block_size: 0x10,
        block_count: 0x30,
    };

    let fat = vec![FATEntry::None; super_block.block_count as usize].into_boxed_slice();

    let mut fs = schema::FileSystem {
        super_block: super_block.clone(),
        fat,
        super_block_cp: super_block,
        obj_lookup: vec![FATEntry::None; 3].into_boxed_slice(),
        rest: RawBytes,
    };

    let fs_size = fs.sourced_size();
    let reserved_blocks = fs_size / fs.super_block.block_size as u64
        + (fs_size % fs.super_block.block_size as u64).min(1);

    fs.fat[0] = FATEntry::Block(reserved_blocks);
    fs.fat[1..reserved_blocks as usize].fill(FATEntry::Reserved);
    for i in reserved_blocks..fs.super_block.block_count - 1 {
        fs.fat[i as usize] = FATEntry::Block(i + 1);
    }
    fs.fat[fs.super_block.block_count as usize - 1] = FATEntry::None;

    fs.encode(data).unwrap();
}

fn main() {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        // .create(true)
        // .truncate(true)
        .open("test.img")
        .unwrap();
    // file.set_len(0x30 * 0x10).unwrap();

    let mut data = StdIO(file);

    // setup(&mut data);

    let mut fs = FileSystem::open(data);

    // fs.create_object(0x55555555555555555555555555555555, 0x20)
    //     .unwrap();

    // fs.write_all(0x55555555555555555555555555555555, &[0x41; 8], 0).unwrap();

    let mut read = [0; 10];
    fs.read_exact(0x55555555555555555555555555555555, &mut read, 0).unwrap();
    println!("{read:?}");

    // fs.disk.seek(SeekFrom::Start(0)).unwrap();
    // println!("{:#?}", schema::FileSystem::decode(&mut fs.disk).unwrap());
    // // for i in 0..fs.frame().unwrap().fat().unwrap().len() {
    // //     println!("{i}: {:?}", fs.frame().unwrap().fat().unwrap().get(i).unwrap());
    // // }
}
