use std::{
    io::{ErrorKind, Read, SeekFrom, Write},
    ptr::null_mut,
    sync::atomic::Ordering,
};

use libc::{S_IFREG, S_IRWXG, S_IRWXO, S_IRWXU};
use twizzler_abi::{
    meta::MetaExt,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_rt_abi::{
    error::ArgumentError,
    fd::FdInfo,
    object::{MapFlags, ObjectCmd, ObjectHandle, MEXT_SIZED},
    Result,
};

use crate::OUR_RUNTIME;

#[derive(Clone)]
pub struct RawFile {
    pub(crate) pos: u64,
    len: u64,
    handle: ObjectHandle,
}

impl RawFile {
    fn update_len(&mut self) {
        if let Some(me) = self.handle.find_meta_ext(MEXT_SIZED) {
            self.len = me.value.load(Ordering::SeqCst);
        }
    }

    pub fn fd_cmd(&mut self, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        match cmd {
            twizzler_rt_abi::bindings::FD_CMD_TRUNCATE => {
                let new_len = unsafe { *(arg as *const u64) };
                self.truncate(new_len)?;
                Ok(())
            }
            _ => Err(ArgumentError::InvalidArgument.into()),
        }
    }

    pub fn open(obj_id: ObjID, flags: MapFlags) -> Result<Self> {
        let handle = OUR_RUNTIME
            .map_object(obj_id, flags | MapFlags::NO_NULLPAGE)
            .unwrap();
        let len = if let Some(me) = handle.find_meta_ext(MEXT_SIZED) {
            me.value.load(Ordering::SeqCst)
        } else {
            if flags.contains(MapFlags::WRITE) {
                unsafe { handle.set_meta_ext(MetaExt::new(MEXT_SIZED, 0))? };
            }
            0
        };
        Ok(Self {
            pos: 0,
            len,
            handle,
        })
    }

    pub fn truncate(&mut self, new_len: u64) -> Result<()> {
        if new_len > (MAX_SIZE - NULLPAGE_SIZE) as u64 {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.len = new_len;
        let me = MetaExt::new(MEXT_SIZED, self.len);
        unsafe { self.handle.set_meta_ext(me)? };
        Ok(())
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<usize> {
        self.update_len();
        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (self.len as i64) - x,
            SeekFrom::Current(x) => (self.pos as i64) + x,
        };

        if new_pos < 0 {
            Err(ArgumentError::InvalidArgument.into())
        } else {
            self.pos = new_pos as u64;
            Ok(self.pos.try_into().unwrap())
        }
    }

    pub fn stat(&mut self) -> Result<FdInfo> {
        self.update_len();
        Ok(FdInfo {
            kind: twizzler_rt_abi::fd::FdKind::Regular,
            size: self.len,
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            id: self.handle.id().raw(),
            unix_mode: S_IFREG | S_IRWXG | S_IRWXO | S_IRWXU,
            accessed: std::time::Duration::ZERO,
            modified: std::time::Duration::ZERO,
            created: std::time::Duration::ZERO,
        })
    }
}

impl Read for RawFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.pread(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl RawFile {
    /// Read up to `buf.len()` bytes from position `offset` without updating `self.pos`.
    pub fn pread(&mut self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        self.update_len();
        if offset >= self.len {
            return Ok(0);
        }
        let copy_len = buf.len().min((self.len - offset) as usize);
        let data = unsafe {
            core::slice::from_raw_parts(self.handle.start().add(offset as usize), copy_len)
        };
        buf[0..copy_len].copy_from_slice(data);
        Ok(copy_len)
    }

    /// Write `buf` at position `offset` without updating `self.pos`.
    pub fn pwrite(&mut self, buf: &[u8], offset: u64) -> std::io::Result<usize> {
        let write_len = buf.len();
        let end_pos = offset + write_len as u64;
        if end_pos > (MAX_SIZE - NULLPAGE_SIZE) as u64 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "write exceeds maximum file size",
            ));
        }
        if end_pos > self.len {
            self.len = end_pos;
            let me = twizzler_rt_abi::object::MetaExt::new(MEXT_SIZED, self.len);
            unsafe { self.handle.set_meta_ext(me)? };
        }
        unsafe {
            let dest = self.handle.start().add(offset as usize);
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dest, write_len);
        }
        self.handle.cmd(ObjectCmd::Sync, null_mut::<()>())?;
        Ok(write_len)
    }
}

impl Write for RawFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.pwrite(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
