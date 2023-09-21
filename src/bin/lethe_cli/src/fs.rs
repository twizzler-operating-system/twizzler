use crate::{
    alloc::SequentialAllocator,
    namer::FsNamer,
    persist::Storage, crypt::Oreo, hash::Hashbrowns
};

use allocator::Allocator;
use embedded_io::{adapters::FromStd, blocking::{Seek, Read, Write}, SeekFrom};
use lethe::{Lethe, LetheBuilder, io::BlockCryptIo};
use fute::shell;
use persistence::PersistentStorage;
use rand::prelude::ThreadRng;
use anyhow::{anyhow, Result};
use khf::{Consolidation};
use fute::file::File;

// Reserved object IDs.
const ALLOCATOR_OBJID: u64 = 4;
const NAMER_OBJID: u64 = 5;

pub struct LetheFute {
    lemosyne: Lethe<FromStd<File>, Storage, SequentialAllocator<u64>, ThreadRng, Oreo, Hashbrowns, 32, 4096>,
    namer: FsNamer,
    allocator: SequentialAllocator<u64>,
}

impl LetheFute {
    pub fn new() -> Result<Self> {
        let mut builder = LetheBuilder::<FromStd<File>, Storage, SequentialAllocator<u64>, ThreadRng, Oreo, Hashbrowns, 32, 4096>::new();

        let file = FromStd::new(File::create("/lethe.enclave")?);
        let x = Storage::new("/lethe.files".to_owned()).expect("Storage should work ):");

        let lemon = builder.build(file, x);

        let mut inner = LetheFute {
            lemosyne: lemon,
            namer: FsNamer::new(),
            allocator: SequentialAllocator::<u64>::new()
        };

        // This assumes we only fail to load if we never persisted it.
        if inner.lemosyne.load_state().is_ok() {

            // Load the allocator.
            let allocator = {
                let mut ser = vec![];
                let mut io = inner.lemosyne.read_handle(&ALLOCATOR_OBJID)?;
                io.read_to_end(&mut ser)?;
                bincode::deserialize(&ser)?
            };

            // Load the namer.
            let namer = {
                let mut ser = vec![];
                let mut io = inner.lemosyne.read_handle(&NAMER_OBJID)?;
                io.read_to_end(&mut ser)?;
                bincode::deserialize(&ser)?
            };

            // Assign the loaded allocator and namer.
            inner.allocator = allocator;
            inner.namer = namer;
        } else {
            // Reserve allocator object ID and create its object.
            inner.allocator.reserve(ALLOCATOR_OBJID)?;
            inner.lemosyne.create(
                &ALLOCATOR_OBJID,
                &0
            )?;

            // Reserve namer object ID and create its object.
            inner.allocator.reserve(NAMER_OBJID)?;
            inner.lemosyne.create(
                &NAMER_OBJID,
                &0
            )?;
        }

                
        Ok(inner)
    }

    pub fn create(&mut self, path: &str) -> Result<()> {
        match self.namer.get(&path.to_owned()) {
            Some(x) => {
                return Ok(())
            },
            None => {
            }
        };
        
        let objid = self.allocator.alloc()?;
        self.namer.insert(path.to_owned(), objid);
        self.lemosyne.create(
            &objid,
            &0
        );

        Ok(())
    }

    pub fn open(&mut self, path: &str) -> Result<()> {
        match self.namer.get(&path.to_owned()) {
            Some(x) => {
                return Ok(())
            },
            None => {
                
            }
        };
        Ok(())
    }

    pub fn read(&mut self, path: &str, buf: &mut [u8], off: usize) -> Result<i32> {
        let objid = self.namer.get(&path.to_owned()).unwrap();
        let mut io = self.lemosyne.read_handle(&objid)?;
        io.seek(SeekFrom::Start(off as u64))?;
        let bytes = io.read(buf)? as i32;
        Ok(bytes)
    }

    pub fn write(&mut self, path: &str, buf: &[u8], off: usize) -> Result<i32> {
        let objid = self.namer.get(&path.to_owned()).unwrap();

        let mut io = self.lemosyne.write_handle(&objid)?;
        io.seek(SeekFrom::Start(off.try_into().unwrap()))?;
    
        Ok(io.write(buf)? as i32)
    }

    pub fn append(&mut self, path: &str, buf: &[u8]) -> Result<i32> {
        let objid = self.namer.get(&path.to_owned()).unwrap();

        let mut io = self.lemosyne.write_handle(&objid)?;
        io.seek(SeekFrom::End(0))?;

        Ok(io.write(buf)? as i32)
    }

    pub fn unlink(&mut self, path: &str) -> Result<i32> {
        let objid = self.namer.remove(&path.to_owned()).unwrap();
        self.allocator.dealloc(objid)?;
        self.lemosyne.destroy(&objid)?;
        
        Ok(0)
    }

    pub fn truncate(&mut self, path: &str, size: usize) -> Result<()> {
        let objid = self.namer.get(&path.to_owned()).unwrap();

        self.lemosyne.truncate(objid, size.try_into().unwrap());

        Ok(())
    }

    pub fn consolidate(&mut self) -> Result<()> {
        self.lemosyne.consolidate_master_khf(Consolidation::Full);

        Ok(())
    }
}

impl Drop for LetheFute {
    fn drop(&mut self) {
        let mut ser = bincode::serialize(&self.allocator).unwrap();
        let mut io = self.lemosyne.write_handle(&ALLOCATOR_OBJID).unwrap();

        io.write_all(&mut ser).unwrap();
        

        let mut ser = bincode::serialize(&self.namer).unwrap();
        let mut io = self.lemosyne.write_handle(&NAMER_OBJID).unwrap();

        io.write_all(&mut ser).unwrap();
    }
}
