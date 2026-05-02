use std::{
    io::ErrorKind,
    sync::{Arc, Mutex},
    time::Duration,
};

use libc::S_IFDIR;
use monitor_api::{CompartmentFlags, CompartmentHandle};
use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_rt_abi::{
    bindings::{IO_REGISTER_SIGNAL, IO_REGISTER_STATUS, STATUS_FLAG_READY, STATUS_FLAG_TERMINATED},
    error::TwzError,
    Result,
};

use crate::runtime::file::Fd;

#[derive(Clone)]
pub struct CompartmentFile {
    inner: Arc<CompartmentFileInner>,
}

struct CompartmentFileInner {
    comp: CompartmentHandle,
    last_state: Mutex<CompartmentFlags>,
}

impl CompartmentFile {
    pub fn new(comp: CompartmentHandle) -> Self {
        let last_state = comp.info().flags;
        Self {
            inner: Arc::new(CompartmentFileInner {
                comp,
                last_state: Mutex::new(last_state),
            }),
        }
    }
}

impl Fd for CompartmentFile {
    fn read(
        &self,
        _buf: &mut [u8],
        flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        let mut current_state = self.inner.comp.info().flags;
        if current_state.contains(CompartmentFlags::EXITED) {
            return Ok(0);
        }
        loop {
            let mut ls = self.inner.last_state.lock().unwrap();

            if current_state != *ls {
                *ls = current_state;
                return Ok(0);
            } else {
                drop(ls);
                if flags.contains(twizzler_rt_abi::io::IoFlags::NONBLOCKING) {
                    return Err(TwzError::WOULD_BLOCK);
                }
                current_state = self.inner.comp.wait(current_state);
            }

            if current_state.contains(CompartmentFlags::EXITED) {
                return Ok(0);
            }
        }
    }

    fn write(
        &self,
        _buf: &[u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        Err(ErrorKind::Unsupported.into())
    }

    fn stat(&self) -> Result<twizzler_rt_abi::fd::FdInfo> {
        let info = self.inner.comp.info();
        Ok(twizzler_rt_abi::fd::FdInfo {
            size: 0,
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            kind: twizzler_rt_abi::fd::FdKind::Compartment,
            id: info.id.raw(),
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: 0o755 | S_IFDIR,
        })
    }

    fn seek(&self, _pos: std::io::SeekFrom) -> Result<usize> {
        Err(ErrorKind::Unsupported.into())
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn fd_cmd(&self, _cmd: u32, _arg: *const u8, _ret: *mut u8) -> Result<()> {
        Ok(())
    }

    fn get_config(&self, reg: u32, val: *mut std::ffi::c_void, val_len: usize) -> Result<()> {
        match reg {
            IO_REGISTER_STATUS => {
                if val_len < std::mem::size_of::<u64>() {
                    Err(TwzError::INVALID_ARGUMENT)
                } else {
                    let info = self.inner.comp.info();
                    let mut status = if info.flags.contains(CompartmentFlags::EXITED) {
                        // Lower 32 bits: exit code; upper bits: status flags.
                        STATUS_FLAG_TERMINATED | (info.exit_code & 0xffff_ffff)
                    } else {
                        0
                    };
                    if info.flags.contains(CompartmentFlags::READY) {
                        status |= STATUS_FLAG_READY;
                    };
                    unsafe {
                        *(val as *mut u64) = status;
                    }
                    Ok(())
                }
            }
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }

    fn set_config(&self, reg: u32, val: *const std::ffi::c_void, val_len: usize) -> Result<()> {
        match reg {
            IO_REGISTER_SIGNAL => {
                if val_len < std::mem::size_of::<u64>() {
                    Err(TwzError::INVALID_ARGUMENT)
                } else {
                    self.inner.comp.signal(unsafe { *val.cast() })?;
                    Ok(())
                }
            }
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }

    fn waitpoint(&self, _kind: twizzler_rt_abi::bindings::wait_kind) -> Result<ThreadSyncSleep> {
        Err(ErrorKind::Unsupported.into())
    }

    fn shutdown(&self, _sh: std::net::Shutdown) -> Result<()> {
        Ok(())
    }
}
