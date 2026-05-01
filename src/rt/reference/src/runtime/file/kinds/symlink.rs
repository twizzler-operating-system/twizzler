use secgate::TwzError;
use twizzler_rt_abi::Result;

use crate::runtime::file::Fd;

pub struct SymLinkFile {
    obj_id: ObjID,
}

impl SymLinkFile {
    pub fn new(obj_id: ObjID) -> Result<Self> {
        Ok(Self { obj_id })
    }
}

impl Fd for SymLinkFile {
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
        todo!()
    }
}
