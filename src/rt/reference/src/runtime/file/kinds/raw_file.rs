use std::{
    io::{ErrorKind, Read, SeekFrom, Write},
    ptr::null_mut,
    sync::atomic::{AtomicU64, Ordering},
};

use libc::{S_IFREG, S_IRWXG, S_IRWXO, S_IRWXU};
use secgate::TwzError;
use twizzler_abi::{
    meta::MetaExt,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::ThreadSyncSleep,
};
use twizzler_rt_abi::{
    error::ArgumentError,
    fd::FdInfo,
    object::{MapFlags, ObjectCmd, ObjectHandle, MEXT_SIZED},
    Result,
};

use crate::{runtime::file::Fd, OUR_RUNTIME};

#[derive(Clone)]
pub struct RawFile {
    pub(crate) pos: AtomicU64,
    len: AtomicU64,
    handle: ObjectHandle,
}

impl RawFile {
    fn update_len(&self) {
        if let Some(me) = self.handle.find_meta_ext(MEXT_SIZED) {
            self.len
                .store(me.value.load(Ordering::SeqCst), Ordering::SeqCst);
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
            pos: AtomicU64::new(0),
            len: AtomicU64::new(len),
            handle,
        })
    }

    pub fn truncate(&self, new_len: u64) -> Result<()> {
        if new_len > (MAX_SIZE - NULLPAGE_SIZE) as u64 {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.len.store(new_len, Ordering::SeqCst);
        let me = MetaExt::new(MEXT_SIZED, new_len);
        unsafe { self.handle.set_meta_ext(me)? };
        Ok(())
    }
}

impl Fd for RawFile {
    fn read(
        &self,
        buf: &mut [u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        a_offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        self.update_len();
        let offset = a_offset.unwrap_or(self.pos.load(Ordering::SeqCst));
        let len = self.len.load(Ordering::SeqCst);
        if offset >= len {
            return Ok(0);
        }
        let copy_len = buf.len().min((len - offset) as usize);
        let data = unsafe {
            core::slice::from_raw_parts(self.handle.start().add(offset as usize), copy_len)
        };
        buf[0..copy_len].copy_from_slice(data);
        if a_offset.is_none() {
            self.pos.store(offset + copy_len as u64, Ordering::SeqCst);
        }
        Ok(copy_len)
    }

    fn write(
        &self,
        buf: &[u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        a_offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        let offset = a_offset.unwrap_or(self.pos.load(Ordering::SeqCst));
        let write_len = buf.len();
        let end_pos = offset + write_len as u64;
        if end_pos > (MAX_SIZE - NULLPAGE_SIZE) as u64 {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let len = self.len.load(Ordering::SeqCst);
        if end_pos > len {
            self.len.store(end_pos, Ordering::SeqCst);
            let me = twizzler_rt_abi::object::MetaExt::new(MEXT_SIZED, end_pos);
            unsafe { self.handle.set_meta_ext(me)? };
        }
        unsafe {
            let dest = self.handle.start().add(offset as usize);
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dest, write_len);
        }
        if a_offset.is_none() {
            self.pos.store(offset + write_len as u64, Ordering::SeqCst);
        }
        self.handle.cmd(ObjectCmd::Sync, null_mut::<()>())?;
        Ok(write_len)
    }

    fn stat(&self) -> Result<FdInfo> {
        self.update_len();
        Ok(FdInfo {
            kind: twizzler_rt_abi::fd::FdKind::Regular,
            size: self.len.load(Ordering::SeqCst),
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            id: self.handle.id().raw(),
            unix_mode: S_IFREG | S_IRWXG | S_IRWXO | S_IRWXU,
            accessed: std::time::Duration::ZERO,
            modified: std::time::Duration::ZERO,
            created: std::time::Duration::ZERO,
        })
    }

    fn seek(&self, pos: SeekFrom) -> Result<usize> {
        self.update_len();
        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (self.len.load(Ordering::SeqCst) as i64) - x,
            SeekFrom::Current(x) => (self.pos.load(Ordering::SeqCst) as i64) + x,
        };

        if new_pos < 0 {
            Err(ArgumentError::InvalidArgument.into())
        } else {
            self.pos.store(new_pos as u64, Ordering::SeqCst);
            Ok(new_pos as usize)
        }
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn fd_cmd(&self, cmd: u32, arg: *const u8, _ret: *mut u8) -> Result<()> {
        match cmd {
            twizzler_rt_abi::bindings::FD_CMD_TRUNCATE => {
                let new_len = unsafe { *(arg as *const u64) };
                self.truncate(new_len)?;
                Ok(())
            }
            _ => Err(ArgumentError::InvalidArgument.into()),
        }
    }

    fn get_config(&self, _reg: u32, _val: *mut std::ffi::c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn set_config(&self, _reg: u32, _val: *const std::ffi::c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn waitpoint(&self, _kind: twizzler_rt_abi::bindings::wait_kind) -> Result<ThreadSyncSleep> {
        if let Some(me) = self.handle.find_meta_ext(MEXT_SIZED) {
            Ok((&me.value, self.pos.load(Ordering::SeqCst)).into())
        } else {
            Err(ErrorKind::Unsupported.into())
        }
    }

    fn shutdown(&self, _sh: std::net::Shutdown) -> Result<()> {
        Ok(())
    }
}
