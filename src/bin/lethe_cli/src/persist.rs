use embedded_io::adapters::FromStd;

use persistence::PersistentStorage;

use fute::{directory, file::File, shell};
use std::io;

pub struct Storage {
    pub path: String,
}

impl Storage {
    pub fn new(path: String) -> std::io::Result<Self> {
        fute::shell::mkdir(&path)?;

        Ok(Self {
            path: path,
        })    
    }

    pub fn object_path(&self, objid: &u64) -> String {
        let path = format!("{}/{}", self.path, objid);

        return path;
    }
}

impl PersistentStorage for Storage {
    type Id = u64;

    type Flags = u8;

    type Info = u8;

    type Error = std::io::Error;

    type Io<'a> = FromStd<File>;

    fn create(&mut self, objid: &Self::Id, flags: &Self::Flags) -> Result<(), Self::Error> {
        let path: String = self.object_path(objid);

        File::create(&path)?;

        Ok(())
    }

    fn destroy(&mut self, objid: &Self::Id) -> Result<(), Self::Error> {
        let path: String = self.object_path(objid);
        println!("Path {}", path);
        fute::shell::rm(&path)?;
        
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
        f.truncate(size).unwrap();

        Ok(())
    }

    fn persist_state(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn load_state(&mut self) -> Result<(), Self::Error> {
        Ok(())
    } 
    
}