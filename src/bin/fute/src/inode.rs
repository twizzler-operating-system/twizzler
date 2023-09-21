use std::io::{Error, ErrorKind};
use twizzler_abi::syscall::{ObjectCreate, BackingType, LifetimeType, ObjectCreateFlags};
use twizzler_object::{ObjID, Object, Protections, ObjectInitFlags};
use crate::constants::MAGIC_NUMBER;

pub type NodeID = ObjID;
#[derive(PartialEq, Eq, Copy, Clone)]
pub enum FileType {
    File,
    Directory
}

pub struct InodeMeta {
    pub magic: u128,
    pub filetype: FileType,
    pub size: usize,
    pub indirect_id: ObjID
}

impl Default for FileType {
    fn default() -> Self {
        Self::File
    }
}

pub struct DirectoryMeta {
    inode: NodeID,
    pub top: usize
}

pub struct FileMeta {
    inode: NodeID
}

pub fn get_inode(id: NodeID) -> Result<Object<InodeMeta>, std::io::Error> {
    match Object::<InodeMeta>::init_id(
        ObjID::from(id),
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    ) {
        Ok(x) => {
            if unsafe {x.base_unchecked().magic != MAGIC_NUMBER} {
                return Err(Error::from(ErrorKind::NotFound))
            }

            Ok(x)
        },
        Err(_) => return Err(Error::from(ErrorKind::NotFound)),
    }
}

fn make_dir_raw(inode: ObjID) -> Result<ObjID, std::io::Error> {
    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Persistent,
        None,
        ObjectCreateFlags::empty(),
    );

    let vecid = twizzler_abi::syscall::sys_object_create(
        create,
        &[],
        &[],
    ).unwrap();

    let obj = Object::<DirectoryMeta>::init_id(
        vecid,
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    ).unwrap();

    unsafe {
        let dir = obj.base_mut_unchecked();
        dir.inode = vecid;
        dir.top = 0;

        Ok(dir.inode)
    }

}

fn make_file_raw(inode: ObjID) -> Result<ObjID, std::io::Error> {
    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Persistent,
        None,
        ObjectCreateFlags::empty(),
    );

    let vecid = twizzler_abi::syscall::sys_object_create(
        create,
        &[],
        &[],
    ).unwrap();

    let obj = Object::<FileMeta>::init_id(
        vecid,
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    ).unwrap();

    unsafe {
        let dir = obj.base_mut_unchecked();
        dir.inode = vecid;

        Ok(dir.inode)
    }
}

pub fn verify_inode(inode: ObjID) -> Result<FileType, std::io::Error> {
    let obj = Object::<InodeMeta>::init_id(
        inode,
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    ).unwrap();

    let node =  unsafe {
        obj.base_unchecked()
    };

    if node.magic != MAGIC_NUMBER {
        return Err(Error::from(ErrorKind::NotFound)) 
    }

    Ok(node.filetype)
}

pub fn create_inode(filetype: FileType) -> Result<(Object<InodeMeta>, ObjID), std::io::Error> {
    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Persistent,
        None,
        ObjectCreateFlags::empty(),
    );

    let vecid = twizzler_abi::syscall::sys_object_create(
        create,
        &[],
        &[],
    ).unwrap();

    let obj = Object::<InodeMeta>::init_id(
        vecid,
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    ).unwrap();

    unsafe {
        let node = obj.base_mut_unchecked();
        node.filetype = filetype;
        node.magic = MAGIC_NUMBER;
        node.size = 0;
        node.indirect_id = match node.filetype {
            FileType::Directory => make_dir_raw(vecid)?,
            FileType::File => make_file_raw(vecid)?,
        };
    };
    Ok((obj, vecid))
}

pub fn inode_filetype(inode: &Object<InodeMeta>) -> Result<FileType, std::io::Error> {
    Ok(unsafe{inode.base_unchecked().filetype})
}

pub fn is_directory(inode: &Object<InodeMeta>) -> Result<(), std::io::Error> {
    if inode_filetype(inode)? != FileType::Directory {return Err(Error::from(ErrorKind::NotADirectory))};

    Ok(())
}

