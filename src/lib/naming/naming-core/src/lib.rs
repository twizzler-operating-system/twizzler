pub mod api;
pub mod dynamic;
pub mod handle;
mod store;

pub const MAX_KEY_SIZE: usize = 256;
pub const PATH_MAX: usize = 4096;

pub type Result<T> = std::result::Result<T, TwzError>;

pub use store::{GetFlags, NameSession, NameStore, NsNode, NsNodeKind};
use twizzler_rt_abi::{bindings::objid as ObjID, error::TwzError};

pub fn objid_to_ino(id: ObjID) -> Option<u32> {
    if id == 1 {
        return Some(0);
    };
    let (hi, lo) = ((id >> 64) as u64, id as u64);
    if hi == (1u64 << 63) {
        let ino = lo & !(1u64 << 63);
        Some(ino as u32)
    } else {
        None
    }
}

pub fn ino_to_objid(ino: u32) -> ObjID {
    if ino == 0 {
        return 1;
    }
    (1u128 << 127) | (ino as u128) | (1u128 << 63)
}

pub const MAX_EXTERNAL_PATH: usize = 4096;
pub const NAME_MAX: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(C)]
pub struct ExternalFile {
    pub id: ObjID,
    pub name: [u8; NAME_MAX],
    pub name_len: u32,
    pub kind: ExternalKind,
}

impl ExternalFile {
    pub fn new(iname: &[u8], kind: ExternalKind, id: ObjID) -> Self {
        let name_len = iname.len().min(NAME_MAX);
        let sname = &iname[0..name_len];
        let mut name = [0; NAME_MAX];
        name[0..name_len].copy_from_slice(&sname);
        Self {
            id,
            name,
            kind,
            name_len: name_len as u32,
        }
    }

    pub fn name(&self) -> Option<&str> {
        str::from_utf8(&self.name[0..(self.name_len as usize)]).ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(u32)]
pub enum ExternalKind {
    Regular,
    Directory,
    SymLink,
    Other,
}
