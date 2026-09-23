use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

use libc::S_IFDIR;
use secgate::TwzError;
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{
    object::{MapFlags, ObjectHandle, MEXT_MTIME},
    Result,
};

use crate::{runtime::file::Fd, OUR_RUNTIME};

pub struct DirFile {
    obj_id: ObjID,
    pos: AtomicU64,
    /// The namespace object, mapped read-only on the first stat and kept for the rest.
    handle: OnceLock<Option<ObjectHandle>>,
}

impl DirFile {
    pub fn new(obj_id: ObjID) -> std::io::Result<Self> {
        Ok(Self {
            obj_id,
            pos: AtomicU64::new(0),
            handle: OnceLock::new(),
        })
    }

    /// Directory mtime in seconds. Native namespaces stamp `MEXT_MTIME` on every entry change;
    /// external ones get it synthesized by the pager from the store inode. Floored to 1 like
    /// regular files, so an unmapped or unstamped directory still stats as existing.
    fn mtime_secs(&self) -> u64 {
        self.handle
            .get_or_init(|| {
                naming_core::namespace_stat_object(self.obj_id)
                    .and_then(|id| OUR_RUNTIME.map_object(id, MapFlags::READ).ok())
            })
            .as_ref()
            .and_then(|h| h.find_meta_ext(MEXT_MTIME))
            .map(|me| me.value.load(Ordering::SeqCst))
            .unwrap_or(0)
            .max(1)
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
            modified: std::time::Duration::from_secs(self.mtime_secs()),
            unix_mode: 0o755 | S_IFDIR,
            nlink: 1,
        })
    }
}
