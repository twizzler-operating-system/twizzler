
use std::{str::from_utf8, env, io::BufRead, ffi::OsStr};


use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{
        BackingType, LifetimeType,
        ObjectCreate, ObjectCreateFlags, sys_object_ctrl,
        ObjectControlCmd, DeleteFlags
    },
};
use std::{io::{Error, ErrorKind}, mem::size_of};
use arrayvec::ArrayString;

use twizzler_object::{Object, ObjectInitFlags};
use crate::{constants::DIRECTORY_MAGIC_NUMBER, inode::{*}};



#[repr(C)]
pub struct DirectoryEntry {
    pub fileno : ObjID,
    pub filename : ArrayString<256>
}

pub fn open_directory(inode: &Object<InodeMeta>) -> Result<Object<DirectoryMeta>, std::io::Error> {
    is_directory(inode)?;

    let x = unsafe {inode.base_unchecked()};
    
    let dir_id = x.indirect_id;

    match Object::<DirectoryMeta>::init_id(
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

pub fn get_root_id() -> NodeID {
    ObjID::from(std::env::args()
        .nth(1).expect("Root ID not found!")
        .parse::<u128>().unwrap())
}

pub fn get_current_id() -> NodeID {
    ObjID::from(std::env::args()
        .nth(2).expect("Root ID not found!")
        .parse::<u128>().unwrap())
}

pub fn create_entry(fileno: ObjID, filename: &str) -> DirectoryEntry {
    DirectoryEntry { 
        fileno:  fileno, 
        filename: ArrayString::from(filename).expect("Capacity Error")
    }
}

fn get_entry_offset(obj: &Object<DirectoryMeta>, index: usize) -> usize{
    obj.slot().vaddr_start() + size_of::<DirectoryMeta>() + size_of::<DirectoryEntry>() * index
}

pub unsafe fn mut_entry_pointer(obj: &Object<DirectoryMeta>, index: usize) -> *mut DirectoryEntry {
    let i = get_entry_offset(obj, index);

    i as *mut DirectoryEntry
}

pub unsafe fn const_entry_pointer(obj: &Object<DirectoryMeta>, index: usize) -> &DirectoryEntry {
    let i = get_entry_offset(obj, index);

    (i as *const DirectoryEntry).as_ref().unwrap()
}


fn set_entry(dir: &Object<DirectoryMeta>, entry: DirectoryEntry, index: usize) -> Result<(), ()> {
    unsafe {
        let p: *mut DirectoryEntry = mut_entry_pointer(dir, index);
        *p = entry;
    }

    Ok(())
}

pub fn push_entry(dir: &Object<DirectoryMeta>, entry: DirectoryEntry) -> Result<(), ()> {
    let top = unsafe {dir.base_mut_unchecked()};
    set_entry(dir, entry, top.top).expect("Couldn't push entry");
    top.top+=1;
    Ok(())
}

pub fn get_entry(dir: &Object<DirectoryMeta>, index: usize) -> Result<&DirectoryEntry, ()> {
    let entry = unsafe {
        const_entry_pointer(dir, index)
    };

    Ok(entry)
}

// Takes the child directory and inode, and the u128 of the parent, and child directory
pub fn set_preset_entries(obj: &Object<DirectoryMeta>, parent: ObjID, current: ObjID) {
    let parent = create_entry(parent, "..");
    let current = create_entry(current, ".");

    push_entry(obj, parent).expect("Failure to write");
    push_entry(obj, current).expect("Failure to write");
}


// Returns the inode of the child directory that was created
pub fn make_dir(parent_inode: &Object<InodeMeta>, name: &str) -> Result<NodeID, std::io::Error> {
    is_directory(parent_inode)?;

    let (parent_dir, parent_id) = (open_directory(parent_inode)?, parent_inode.id());
    match search_directory(&parent_dir, name) {
        Ok(_) => {return Err(Error::from(ErrorKind::AlreadyExists))},
        Err(x) => ()

    }
    let (child_node, child_id) = create_inode(FileType::Directory)?;
    let child_dir = open_directory(&child_node)?;

    push_entry(&parent_dir, create_entry(child_id, name)).unwrap();
    set_preset_entries(&child_dir, parent_id, child_id);

    Ok(child_id)
}

fn is_directory(node: &Object<InodeMeta>) -> Result<(), std::io::Error> {
    if unsafe {node.base_unchecked().filetype != FileType::Directory} {
        return Err(Error::from(ErrorKind::NotADirectory)); 
    }

    Ok(())
}

// Returns the ObjID of the inode which matches the name 
pub fn search_directory(inode: &Object<DirectoryMeta>, name: &str) -> Result<NodeID, std::io::Error> {
    let top = unsafe {inode.base_unchecked().top};

    for i in 0..top {
        let entry = get_entry(&inode, i).expect("Directory Entry isn't valid");
        if entry.filename.as_bytes() == name.as_bytes() {
            return Ok(entry.fileno);
        }
    }

    Err(Error::from(ErrorKind::NotFound))
}

pub fn remove_entry(inode: &Object<DirectoryMeta>, name: &str) -> Result<(), std::io::Error> {
    let top = unsafe{inode.base_unchecked().top};

    for i in 2..top {
        let entry = get_entry(&inode, i).expect("Directory Entry isn't valid");
        if entry.filename.as_bytes() == name.as_bytes() {
            let a = get_entry(&inode, top - 1).unwrap();
            set_entry(inode, create_entry(a.fileno, a.filename.as_str()), i).unwrap();
            unsafe{inode.base_mut_unchecked().top -= 1;}
            return Ok(());
        }
    }

    return Err(Error::from(ErrorKind::NotFound))
}

// Gets the inode which you want to modify 
pub fn namei_raw(root: NodeID, current:  NodeID, path: Vec<&OsStr>) -> Result<Object<InodeMeta>, std::io::Error> {
    let mut node_id = match path[0] == "/" {
        true => root,
        false => current
    };

    let mut is_file = false;
    if path.len() > 0 {
        for s in path {
            if is_file {
                return Err(Error::from(ErrorKind::NotADirectory))
            }
            let s = s.to_str().unwrap();
            
            if s == "/" {continue};
            let dir_node = get_inode(node_id)?;
    
            node_id = match inode_filetype(&dir_node)? {
                FileType::Directory => {
                    let dir: Object<DirectoryMeta> = open_directory(&dir_node)?;
                    let entry = search_directory(&dir, s)?;
    
                    entry
                },
                FileType::File => {
                    is_file = true;
                    node_id
                }
            };
        }
    }

    Ok(get_inode(node_id)?)
}