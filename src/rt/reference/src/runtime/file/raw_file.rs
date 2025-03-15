use std::io::{ErrorKind, Read, SeekFrom, Write};

use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::{
    fd::FdInfo,
    object::{MapFlags, ObjectHandle, MEXT_SIZED},
};

use super::FdKind;
use crate::OUR_RUNTIME;

#[derive(Clone)]
pub struct RawFile {
    pos: u64,
    len: u64,
    handle: ObjectHandle,
}

impl RawFile {
    pub fn open(obj_id: ObjID, flags: MapFlags) -> std::io::Result<Self> {
        let handle = OUR_RUNTIME.map_object(obj_id, flags).unwrap();
        let len = handle
            .find_meta_ext(MEXT_SIZED)
            .map(|me| me.value)
            .unwrap_or((MAX_SIZE - NULLPAGE_SIZE * 2) as u64);
        Ok(Self {
            pos: 0,
            len,
            handle,
        })
    }

    pub fn seek(&mut self, pos: SeekFrom) -> std::io::Result<usize> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (self.len as i64) - x,
            SeekFrom::Current(x) => (self.pos as i64) + x,
        };

        if new_pos < 0 {
            Err(ErrorKind::InvalidInput.into())
        } else {
            self.pos = new_pos as u64;
            Ok(self.pos.try_into().unwrap())
        }
    }

    pub fn stat(&self) -> std::io::Result<FdInfo> {
        Ok(FdInfo {
            kind: twizzler_rt_abi::fd::FdKind::Regular,
            size: self.len,
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            id: self.handle.id().raw(),
        })
    }
}

impl Read for RawFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let copy_len = buf.len().min((self.len - self.pos) as usize);
        let data = unsafe {
            core::slice::from_raw_parts(
                self.handle.start().add(NULLPAGE_SIZE + self.pos as usize),
                copy_len,
            )
        };
        buf[0..copy_len].copy_from_slice(&data);
        self.pos += copy_len as u64;
        Ok(copy_len)
    }
}

impl Write for RawFile {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(ErrorKind::Unsupported.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
