use twizzler_abi::{
    object::{ObjID, Protections, NULLPAGE_SIZE},
    syscall::{
        BackingType, LifetimeType,
        ObjectCreate, ObjectCreateFlags, sys_object_ctrl,
        ObjectControlCmd, DeleteFlags, ObjectControlError
    },
};
use twizzler_object::{Object, ObjectInitFlags, ObjectInitError};
use std::{io::{Error, ErrorKind}, mem::size_of, ffi::OsStr, path::PathBuf};
use std::io::SeekFrom;

use crate::{inode::{FileMeta, InodeMeta, FileType, create_inode}, directory::{get_root_id, get_current_id, namei_raw, open_directory, create_entry, push_entry}};

use std::io::{Read, Write, Seek};

pub struct File {
    obj: Object<InodeMeta>,
    pointer: usize,
    data: Object<FileMeta>
}

fn is_file(node: &Object<InodeMeta>) -> Result<(), std::io::Error> {
    if unsafe {node.base_unchecked().filetype != FileType::File} {
        return Err(Error::from(ErrorKind::NotADirectory)); 
    }

    Ok(())
}

fn open_file(inode: &Object<InodeMeta>) -> Result<Object<FileMeta>, std::io::Error> {
    is_file(inode)?;

    let x = unsafe {inode.base_unchecked()};
    
    let dir_id = x.indirect_id;

    match Object::<FileMeta>::init_id(
        ObjID::from(dir_id),
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    ) {
        Ok(x) => {
            Ok(x)
        },
        Err(_) => return Err(Error::from(ErrorKind::NotFound)),
    }
}

impl File {
    pub fn create(path: &str) -> Result<File, Error> {
        let (root, current) = (get_root_id(), get_current_id());
        
        let binding = PathBuf::from(path);
        let mut path : Vec<&OsStr> = binding.iter().collect();

        if path.len() == 1 && (path[0] == "/" || path[0] == "." || path[0] == "..") {
            return Err(Error::from(ErrorKind::IsADirectory)); 
        }
        else if path.len() == 0 {
            return Err(Error::from(ErrorKind::InvalidInput)); 
        }
    
        let file = path.pop().unwrap().to_str().unwrap();
    
        if path.len() == 0 {
            path.push(OsStr::new("."));
        }

        let node = namei_raw(root, current, path)?;

        let dir = open_directory(&node)?;

        let (file_node, file_id) = create_inode(FileType::File)?;

        push_entry(&dir, create_entry(file_id, file)).expect("File making failed :(");

        let file = open_file(&file_node)?;

        Ok(File {
            obj: file_node,
            pointer: 0,
            data: file,
        })
    }

    pub fn open(path: &str) -> Result<File, Error> {
        let (root, current) = (get_root_id(), get_current_id());
        
        let binding = PathBuf::from(path);
        let mut path : Vec<&OsStr> = binding.iter().collect();

        let node = namei_raw(root, current, path)?;

        let file = open_file(&node)?;

        Ok(File {
            obj: node,
            pointer: 0,
            data: file,
        })
    }

    pub fn append(path: &str)  -> Result<File, Error> {
        let mut x = Self::open(path)?;

        x.seek(SeekFrom::End(0))?;

        Ok(x)
    }

    pub fn close(self) {

    }

    pub fn truncate(&mut self, size: u64) -> Result<(), Error> {
        unsafe {self.obj.base_mut_unchecked().size = size as usize};
        Ok(())
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let meta = unsafe {self.obj.base_unchecked()};

        let max = meta.size - self.pointer;
            
        let bytes_read = match max >= buf.len() {
            true => buf.len(),
            false => max  
        };

        unsafe {
            let p = self.data.slot().vaddr_start() + size_of::<FileMeta>();

            for i in 0..bytes_read {
                let x = ((p + self.pointer) as *const u8).as_ref().unwrap();
                buf[i] = *x;
                self.pointer+=1;
            }
        }


        Ok(bytes_read)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        let meta = unsafe {self.obj.base_unchecked()};

        if self.pointer + buf.len() <= meta.size {
            self.read(buf)?;
        }
        else {
            return Err(Error::from(ErrorKind::UnexpectedEof));
        }

        Ok(())
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        let meta = unsafe {self.obj.base_mut_unchecked()};

        let mut bytes_written = match self.pointer + buf.len() > 0x40000000 {
            true => 0x40000000 - self.pointer,
            false => buf.len()
        };

        if bytes_written <= 0 {
            return Ok(0);
        }

        unsafe {
            let p = self.data.slot().vaddr_start() + size_of::<FileMeta>();

            for i in 0..bytes_written {
                let x = ((p + self.pointer) as *mut u8).as_mut().unwrap();
                *x = buf[i];
                self.pointer+=1;
            }

            if self.pointer > meta.size {
                meta.size = self.pointer;
            }
        }

        Ok(bytes_written)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }


}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        let meta = unsafe {self.obj.base_mut_unchecked()};

        match pos {
            SeekFrom::Start(x) => {
                let offset = x as usize;
                if meta.size < offset {
                    meta.size = offset;
                }
                self.pointer = offset;
            },
            SeekFrom::End(x) => {
                self.pointer = meta.size;

                let i = x + self.pointer as i64;
                if i < 0 {
                    return Err(Error::from(ErrorKind::InvalidData));
                }
                if i as usize > meta.size {
                    meta.size = i as usize;
                }

                self.pointer = (i) as usize;
            },
            SeekFrom::Current(x) => {
                let i = x + self.pointer as i64;
                if (i) < 0 {
                    return Err(Error::from(ErrorKind::InvalidData));
                }
                if i as usize > meta.size {
                    meta.size = i as usize;
                }

                self.pointer = (i) as usize;
            },
        }

        Ok(self.pointer.try_into().unwrap())
    }
}