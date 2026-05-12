use std::sync::atomic::{AtomicU64, Ordering};

use libc::S_IFDIR;
use secgate::TwzError;
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::Result;

use crate::runtime::file::Fd;

pub struct DirFile {
    obj_id: ObjID,
    pos: AtomicU64,
}

impl DirFile {
    pub fn new(obj_id: ObjID) -> std::io::Result<Self> {
        Ok(Self {
            obj_id,
            pos: AtomicU64::new(0),
        })
    }
}

impl Fd for DirFile {
    fn read(
        &self,
        _buf: &mut [u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn write(
        &self,
        _buf: &[u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn seek(&self, pos: std::io::SeekFrom) -> Result<usize> {
        let new_pos = match pos {
            std::io::SeekFrom::Start(off) => off,
            std::io::SeekFrom::End(off) => {
                if off < 0 {
                    self.pos
                        .load(Ordering::SeqCst)
                        .checked_sub((-off) as u64)
                        .ok_or(TwzError::INVALID_ARGUMENT)?
                } else {
                    self.pos
                        .load(Ordering::SeqCst)
                        .checked_add(off as u64)
                        .ok_or(TwzError::INVALID_ARGUMENT)?
                }
            }
            std::io::SeekFrom::Current(off) => {
                if off < 0 {
                    self.pos
                        .load(Ordering::SeqCst)
                        .checked_sub((-off) as u64)
                        .ok_or(TwzError::INVALID_ARGUMENT)?
                } else {
                    self.pos
                        .load(Ordering::SeqCst)
                        .checked_add(off as u64)
                        .ok_or(TwzError::INVALID_ARGUMENT)?
                }
            }
        };
        self.pos.store(new_pos, Ordering::SeqCst);
        Ok(new_pos as usize)
    }

    fn stat(&self) -> Result<twizzler_rt_abi::fd::FdInfo> {
        Ok(twizzler_rt_abi::fd::FdInfo {
            size: 4096,
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            kind: twizzler_rt_abi::fd::FdKind::Directory,
            id: self.obj_id.raw(),
            created: std::time::Duration::ZERO,
            accessed: std::time::Duration::ZERO,
            modified: std::time::Duration::ZERO,
            unix_mode: 0o755 | S_IFDIR,
        })
    }
}
