use embedded_io::adapters::FromStd;
use fute::file::File;
use fute::shell::mkdir;
use persistence::PersistentStorage;

use std::{
    io
};

use fute::inode::FileType;
use fute::directory::{get_root_id, get_current_id};

pub struct Storage {
    pub path: String,
    pub root: u128,
    pub working: u128
}

impl Storage {
    pub fn new(path: String) -> std::io::Result<Self> {
        let (root, working) = (get_root_id().as_u128(), get_current_id().as_u128());
        mkdir(root, working, &path);

        Ok(Self {
            path: path,
            root: root,
            working: working,
        })    
    }

    pub fn object_path(&self, objid: &u64) -> String {
        let path = format!("{}/{}", self.path, objid);

        return path;
    }
}

impl PersistentStorage for Storage {
    type Id = u64;

    type Flags = FileType;

    type Info = u8;

    type Error = io::Error;

    type Io<'a> = FromStd<File>;

    fn create(&mut self, objid: &Self::Id, flags: &Self::Flags) -> Result<(), Self::Error> {
        let path: String = self.object_path(objid);

        println!("path: {}", path);
        File::create(&path);

        Ok(())
    }

    fn destroy(&mut self, objid: &Self::Id) -> Result<(), Self::Error> {
        let path: String = self.object_path(objid);

        fute::shell::rm(self.root, self.working, &path);
        
        Ok(())
    }

    fn get_info(&mut self, objid: &Self::Id) -> Result<Self::Info, Self::Error> {
        Ok(0)
    }

    fn set_info(&mut self, objid: &Self::Id, info: Self::Info) -> Result<(), Self::Error> {
        Ok(())
    }

    fn read_handle(&mut self, objid: &Self::Id) -> Result<Self::Io<'_>, Self::Error> {
        let path: String = self.object_path(objid);

        let f = File::open(&path)?;
        let x = FromStd::new(f);
        Ok(x)
    }

    fn write_handle(&mut self, objid: &Self::Id) -> Result<Self::Io<'_>, Self::Error> {
        let path: String = self.object_path(objid);

        let f = File::open(&path)?;
        let x = FromStd::new(f);
        Ok(x)
    }

    fn rw_handle(&mut self, objid: &Self::Id) -> Result<Self::Io<'_>, Self::Error> {
        let path: String = self.object_path(objid);

        let f = File::open(&path)?;
        let x = FromStd::new(f);
        Ok(x)    
    }

    fn truncate(&mut self, objid: &Self::Id, size: u64) -> Result<(), Self::Error> {
        let path: String = self.object_path(objid);
        let mut f = File::open(&path)?;
        f.truncate(size);

        Ok(())
    }

    fn persist_state(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn load_state(&mut self) -> Result<(), Self::Error> {
        Ok(())
    } 
    
}