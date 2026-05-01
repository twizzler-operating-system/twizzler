use secgate::TwzError;
use twizzler_rt_abi::Result;

use crate::runtime::file::Fd;

pub struct DirFile {
    obj_id: ObjID,
    pos: u64,
}

impl DirFile {
    pub fn new(obj_id: ObjID) -> std::io::Result<Self> {
        Ok(Self { obj_id, pos: 0 })
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

    fn stat(&self) -> Result<twizzler_rt_abi::fd::FdInfo> {
        Ok(twizzler_rt_abi::fd::FdInfo {
            size: 0,
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            kind: twizzler_rt_abi::fd::FdKind::Dir,
            id: self.obj_id,
            created: std::time::Duration::ZERO,
            accessed: std::time::Duration::ZERO,
            modified: std::time::Duration::ZERO,
            unix_mode: 0o755 | S_IFDIR,
        })
    }
}
