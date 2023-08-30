use crate::{
    alloc::SequentialAllocator,
    namer::FsNamer,
    persist::Storage, crypt::{Oreo, Water}, hash::Hashbrowns
};

use allocator::Allocator;
use embedded_io::{adapters::FromStd, blocking::{Seek, Read, Write}, SeekFrom};
use fute::{file::File, inode::FileType};
use lethe::{Lethe, LetheBuilder};
use persistence::PersistentStorage;
use rand::prelude::ThreadRng;
use anyhow::{anyhow, Result};

pub struct LetheFute {
    lemosyne: Lethe<FromStd<File>, Storage, SequentialAllocator<u64>, ThreadRng, Water, Hashbrowns, 32, 4096>,
    namer: FsNamer,
    allocator: SequentialAllocator<u64>
}

impl LetheFute {
    pub fn new() -> Self {
        let mut builder = LetheBuilder::<FromStd<File>, Storage, SequentialAllocator<u64>, ThreadRng, Water, Hashbrowns, 32, 4096>::new();

        File::create("/lethe.hangout");
        
        let file = FromStd::new(File::open("/lethe.hangout").expect("File should have been created :("));
        let x = Storage::new("/lethe.files/".to_owned()).expect("Storage should work ):");

        println!("Building...");
        let lemon = builder.build(file, x);
        println!("Built.");
        LetheFute {
            lemosyne: lemon,
            namer: FsNamer::new(),
            allocator: SequentialAllocator::<u64>::new()
        }
    }

    pub fn create(&mut self, path: &str) -> Result<()> {
        let objid = self.allocator.alloc()?;
        self.namer.insert(path.to_owned(), objid);

        self.lemosyne.create(
            &objid,
            &FileType::File
        );

        Ok(())
    }

    pub fn read(&mut self, path: &str, buf: &mut [u8]) -> Result<i32> {
        let objid = self.namer.get(&path.to_owned()).unwrap();
        let mut io = self.lemosyne.read_handle(&objid)?;
        io.seek(SeekFrom::Start(0))?;
        println!("Reading!");
        Ok(io.read(buf)? as i32)
    }

    pub fn write(&mut self, path: &str, buf: &[u8]) -> Result<i32> {
        let objid = self.namer.get(&path.to_owned()).unwrap();
        let mut io = self.lemosyne.write_handle(&objid)?;
        io.seek(SeekFrom::Start(0))?;
        Ok(io.write(buf)? as i32)
    }

    pub fn unlink(&mut self, path: &str) -> Result<i32> {
        let objid = self.namer.remove(&path.to_owned()).unwrap();

        self.allocator.dealloc(objid)?;
        self.lemosyne.destroy(&objid)?;
        Ok(0)
    }
}