use std::{os::raw::c_void, time::Duration};

use libc::{termios, S_IFCHR};
use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_io::{
    pipe::Pipe,
    pty::{PtyClientHandle, PtyServerHandle},
};
use twizzler_rt_abi::{
    bindings::{wait_kind, IO_REGISTER_TERMIOS, WAIT_WRITE},
    error::TwzError,
    fd::FdFlags,
    io::IoFlags,
    Result,
};

use crate::runtime::file::Fd;

#[derive(Clone)]
pub enum PtyHandleKind {
    Server(PtyServerHandle),
    Client(PtyClientHandle),
}

impl Fd for PtyHandleKind {
    fn read(
        &self,
        buf: &mut [u8],
        flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        if flags.contains(IoFlags::NONBLOCKING) {
            match self {
                PtyHandleKind::Server(server) => server.clone().read_nb(buf).into(),
                PtyHandleKind::Client(client) => client.clone().read_nb(buf).into(),
            }
        } else {
            match self {
                PtyHandleKind::Server(server) => server.clone().read(buf).into(),
                PtyHandleKind::Client(client) => client.clone().read(buf).into(),
            }
        }
    }

    fn write(
        &self,
        buf: &[u8],
        flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        if flags.contains(IoFlags::NONBLOCKING) {
            match self {
                PtyHandleKind::Server(server) => server.clone().write_nb(buf).into(),
                PtyHandleKind::Client(client) => client.clone().write_nb(buf).into(),
            }
        } else {
            match self {
                PtyHandleKind::Server(server) => server.clone().write(buf).into(),
                PtyHandleKind::Client(client) => client.clone().write(buf).into(),
            }
        }
    }

    fn stat(&self) -> Result<twizzler_rt_abi::fd::FdInfo> {
        Ok(twizzler_rt_abi::fd::FdInfo {
            size: 0,
            kind: twizzler_rt_abi::fd::FdKind::PtyHandle,
            flags: FdFlags::IS_TERMINAL,
            id: 0,
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: S_IFCHR | 0o666,
        })
    }

    fn seek(&self, _pos: std::io::SeekFrom) -> Result<usize> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn flush(&self) -> Result<()> {
        match self {
            PtyHandleKind::Server(server) => server.clone().flush().into(),
            PtyHandleKind::Client(client) => client.clone().flush().into(),
        }
    }

    fn fd_cmd(&self, _cmd: u32, _arg: *const u8, _ret: *mut u8) -> Result<()> {
        Ok((()))
    }

    fn get_config(&self, reg: u32, val: *mut std::ffi::c_void, val_len: usize) -> Result<()> {
        match reg {
            IO_REGISTER_TERMIOS => {
                if val_len < size_of::<termios>() {
                    return Err(TwzError::INVALID_ARGUMENT);
                }
                let termios = match self {
                    PtyHandleKind::Server(pty_server_handle) => pty_server_handle.get_termios()?,
                    PtyHandleKind::Client(pty_client_handle) => pty_client_handle.get_termios()?,
                };
                unsafe { (val as *mut termios).write(termios) };
                Ok(())
            }
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }

    fn set_config(&self, reg: u32, val: *const std::ffi::c_void, val_len: usize) -> Result<()> {
        match reg {
            IO_REGISTER_TERMIOS => {
                if val_len < size_of::<termios>() {
                    return Err(TwzError::INVALID_ARGUMENT);
                }
                match self {
                    PtyHandleKind::Server(pty_server_handle) => {
                        pty_server_handle.set_termios(unsafe { val.cast::<termios>().read() })
                    }
                    PtyHandleKind::Client(pty_client_handle) => {
                        pty_client_handle.set_termios(unsafe { val.cast::<termios>().read() })
                    }
                }
                Ok(())
            }
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }

    fn waitpoint(&self, kind: twizzler_rt_abi::bindings::wait_kind) -> Result<ThreadSyncSleep> {
        Ok(match self {
            PtyHandleKind::Server(server) => server.waitpoint(kind == WAIT_WRITE),
            PtyHandleKind::Client(client) => client.waitpoint(kind == WAIT_WRITE),
        })
    }

    fn shutdown(&self, _sh: std::net::Shutdown) -> Result<()> {
        Ok(())
    }
}

impl Fd for Pipe {
    fn read(
        &self,
        buf: &mut [u8],
        flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        self.read(buf, flags.contains(IoFlags::NONBLOCKING)).into()
    }

    fn write(
        &self,
        buf: &[u8],
        flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        self.write(buf, flags.contains(IoFlags::NONBLOCKING)).into()
    }

    fn stat(&self) -> Result<twizzler_rt_abi::fd::FdInfo> {
        Ok(twizzler_rt_abi::fd::FdInfo {
            size: 0,
            kind: twizzler_rt_abi::fd::FdKind::Pipe,
            flags: FdFlags::empty(),
            id: 0,
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: S_IFCHR | 0o666,
        })
    }

    fn seek(&self, _pos: std::io::SeekFrom) -> Result<usize> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn flush(&self) -> Result<()> {
        self.flush().into()
    }

    fn fd_cmd(&self, _cmd: u32, _arg: *const u8, _ret: *mut u8) -> Result<()> {
        Ok((()))
    }

    fn get_config(&self, _reg: u32, _val: *mut std::ffi::c_void, _val_len: usize) -> Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn set_config(&self, _reg: u32, _val: *const std::ffi::c_void, _val_len: usize) -> Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn waitpoint(&self, kind: twizzler_rt_abi::bindings::wait_kind) -> Result<ThreadSyncSleep> {
        if kind == WAIT_WRITE {
            Ok(self.pipe.base().buffer.write_waitpoint())
        } else {
            Ok(self.pipe.base().buffer.read_waitpoint())
        }
    }

    fn shutdown(&self, sh: std::net::Shutdown) -> Result<()> {
        if matches!(sh, std::net::Shutdown::Read) || matches!(sh, std::net::Shutdown::Both) {
            self.close_reader();
        }
        if matches!(sh, std::net::Shutdown::Write) || matches!(sh, std::net::Shutdown::Both) {
            self.close_writer();
        }
        Ok(())
    }
}
