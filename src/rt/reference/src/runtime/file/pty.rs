use std::{
    io::{Read, Write},
    os::raw::c_void,
};

use libc::termios;
use twizzler_io::pty::{PtyClientHandle, PtyServerHandle};
use twizzler_rt_abi::{bindings::IO_REGISTER_TERMIOS, error::TwzError};

#[derive(Clone)]
pub enum PtyHandleKind {
    Server(PtyServerHandle),
    Client(PtyClientHandle),
}

impl PtyHandleKind {
    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            PtyHandleKind::Server(server) => server.clone().read(buf),
            PtyHandleKind::Client(client) => client.clone().read(buf),
        }
    }

    pub fn set_config(&self, reg: u32, val: *const c_void, val_len: usize) -> Result<(), TwzError> {
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
}

impl PtyHandleKind {
    pub fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            PtyHandleKind::Server(server) => server.clone().write(buf),
            PtyHandleKind::Client(client) => client.clone().write(buf),
        }
    }

    pub fn flush(&self) -> std::io::Result<()> {
        match self {
            PtyHandleKind::Server(server) => server.clone().flush(),
            PtyHandleKind::Client(client) => client.clone().flush(),
        }
    }
}
