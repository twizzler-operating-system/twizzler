mod engine;
mod smoltcp;

use std::{
    net::{SocketAddr, ToSocketAddrs},
    os::raw::c_void,
    sync::{Arc, OnceLock},
    time::Duration,
};

use libc::S_IFSOCK;
use secgate::TwzError;
pub use smoltcp::{dns, SmolTcpListener, SmolTcpStream};
use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_rt_abi::{
    bindings::{
        wait_kind, IO_REGISTER_ADDR, IO_REGISTER_PEER, IO_REGISTER_SOCKET_FLAGS, WAIT_READ,
        WAIT_WRITE,
    },
    fd::{FdFlags, SocketAddress},
    io::IoFlags,
    Result,
};

use crate::runtime::file::{kinds::socket::smoltcp::UdpSocket, Fd};

#[derive(Clone)]
pub enum SocketKind {
    TcpStream(Arc<SmolTcpStream>),
    TcpListener(Arc<SmolTcpListener>),
    UdpSocket(OnceLock<Arc<UdpSocket>>),
}

impl SocketKind {
    fn get_endpoint_addr(&self, peer: bool) -> Result<SocketAddress> {
        match self {
            SocketKind::TcpStream(smol_tcp_stream) => Ok(smol_tcp_stream.addr(peer)),
            SocketKind::TcpListener(smol_tcp_listener) => Ok(smol_tcp_listener.addr(peer)),
            SocketKind::UdpSocket(udp_socket) => Ok(udp_socket
                .get()
                .ok_or(TwzError::INVALID_ARGUMENT)?
                .addr(peer)),
        }
    }

    fn get_socket_flags(&self) -> Result<u32> {
        match self {
            SocketKind::TcpStream(smol_tcp_stream) => Ok(smol_tcp_stream.flags()),
            SocketKind::TcpListener(smol_tcp_listener) => Ok(smol_tcp_listener.flags()),
            SocketKind::UdpSocket(udp_socket) => {
                Ok(udp_socket.get().ok_or(TwzError::INVALID_ARGUMENT)?.flags())
            }
        }
    }

    fn set_socket_flags(&self, flags: u32) -> Result<()> {
        match self {
            SocketKind::TcpStream(smol_tcp_stream) => Ok(smol_tcp_stream.set_flags(flags)),
            SocketKind::TcpListener(smol_tcp_listener) => Ok(smol_tcp_listener.set_flags(flags)),
            SocketKind::UdpSocket(udp_socket) => Ok(udp_socket
                .get()
                .ok_or(TwzError::INVALID_ARGUMENT)?
                .set_flags(flags)),
        }
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        tracing::debug!(
            "Binding TCP socket to address {:?}",
            addr.to_socket_addrs().map(|mut x| x.next())
        );
        SmolTcpListener::bind(addr)
            .map(|listener| SocketKind::TcpListener(Arc::new(listener)))
            .map_err(Into::into)
    }

    pub fn udp_bind<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        UdpSocket::bind(addr)
            .map(|listener| SocketKind::UdpSocket(OnceLock::from(Arc::new(listener))))
            .map_err(Into::into)
    }

    pub fn accept(&self) -> Result<Self> {
        tracing::debug!("Accepting on socket");
        match self {
            SocketKind::TcpListener(listener) => listener
                .accept(IoFlags::empty())
                .map(|stream| SocketKind::TcpStream(Arc::new(stream.0)))
                .map_err(Into::into),
            _ => Err(TwzError::NOT_SUPPORTED),
        }
    }

    pub fn udp_connect<A: ToSocketAddrs>(&self, addr: A) -> Result<()> {
        match self {
            SocketKind::UdpSocket(udp_socket) => Ok(udp_socket
                .get()
                .ok_or(TwzError::INVALID_ARGUMENT)?
                .connect(addr)?),
            _ => panic!("invalid socket type"),
        }
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        tracing::info!(
            "Connecting to socket at address {:?}",
            addr.to_socket_addrs().map(|mut x| x.next())
        );
        SmolTcpStream::connect(IoFlags::empty(), addr)
            .map(|stream| SocketKind::TcpStream(Arc::new(stream)))
            .map_err(Into::into)
    }

    pub fn is_ready(&self, kind: wait_kind) -> bool {
        match kind {
            x if x == WAIT_READ => match self {
                SocketKind::TcpStream(smol_tcp_stream) => smol_tcp_stream.can_read(),
                SocketKind::TcpListener(smol_tcp_listener) => smol_tcp_listener.can_read(),
                SocketKind::UdpSocket(udp_socket) => {
                    udp_socket.get().map_or(false, |s| s.can_read())
                }
            },
            x if x == WAIT_WRITE => match self {
                SocketKind::TcpStream(smol_tcp_stream) => smol_tcp_stream.can_write(),
                SocketKind::TcpListener(smol_tcp_listener) => smol_tcp_listener.can_write(),
                SocketKind::UdpSocket(udp_socket) => {
                    udp_socket.get().map_or(false, |s| s.can_write())
                }
            },
            _ => false,
        }
    }
}

impl Fd for SocketKind {
    fn read(
        &self,
        buf: &mut [u8],
        flags: IoFlags,
        _offset: Option<u64>,
        ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        if let Some(ep) = ep {
            match self {
                SocketKind::UdpSocket(stream) => {
                    let val = stream
                        .get()
                        .ok_or(TwzError::INVALID_ARGUMENT)?
                        .read_from(buf, flags)?;
                    if let Some(addr) = val.1.map(|x| x.endpoint) {
                        let sa = SocketAddr::from((addr.addr, addr.port));
                        let sa = twizzler_rt_abi::fd::SocketAddress::from(sa);
                        *ep = sa.into();
                    }
                    Ok(val.0)
                }
                _ => Err(TwzError::NOT_SUPPORTED),
            }
        } else {
            match self {
                SocketKind::TcpStream(stream) => stream.read(buf, flags).map_err(Into::into),
                SocketKind::UdpSocket(stream) => stream
                    .get()
                    .ok_or(TwzError::INVALID_ARGUMENT)?
                    .read(buf, flags)
                    .map_err(Into::into),
                _ => Err(TwzError::NOT_SUPPORTED),
            }
        }
    }

    fn write(
        &self,
        buf: &[u8],
        flags: IoFlags,
        _offset: Option<u64>,
        to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        if let Some(to) = to {
            let sa = twizzler_rt_abi::fd::SocketAddress::try_from(*to)?;
            let sa = SocketAddr::from(sa);
            match self {
                SocketKind::UdpSocket(stream) => {
                    let stream = stream.get_or_try_init(|| {
                        Ok::<_, std::io::Error>(Arc::new(UdpSocket::bind_ephemeral(sa)?))
                    })?;
                    stream.write_to(buf, sa.into(), flags)?;
                    Ok(buf.len())
                }
                _ => Err(TwzError::NOT_SUPPORTED),
            }
        } else {
            match self {
                SocketKind::TcpStream(stream) => stream.write(buf, flags).map_err(Into::into),
                SocketKind::UdpSocket(stream) => stream
                    .get()
                    .ok_or(TwzError::INVALID_ARGUMENT)?
                    .write(buf, flags)
                    .map(|_| buf.len())
                    .map_err(Into::into),
                _ => Err(TwzError::NOT_SUPPORTED),
            }
        }
    }

    fn stat(&self) -> Result<twizzler_rt_abi::fd::FdInfo> {
        Ok(twizzler_rt_abi::fd::FdInfo {
            kind: twizzler_rt_abi::fd::FdKind::Socket.into(),
            size: 0,
            flags: FdFlags::empty(),
            id: 0,
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: S_IFSOCK | 0o777,
        })
    }

    fn seek(&self, _pos: std::io::SeekFrom) -> Result<usize> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn flush(&self) -> Result<()> {
        match self {
            SocketKind::TcpStream(stream) => stream.flush().map_err(Into::into),
            SocketKind::UdpSocket(stream) => stream
                .get()
                .ok_or(TwzError::INVALID_ARGUMENT)?
                .flush()
                .map_err(Into::into),
            _ => Ok(()),
        }
    }

    fn fd_cmd(&self, _cmd: u32, _arg: *const u8, _ret: *mut u8) -> Result<()> {
        Ok(())
    }

    fn set_config(&self, reg: u32, val: *const c_void, val_len: usize) -> Result<()> {
        fn read_data<T>(ptr: *const c_void, val_len: usize) -> Result<T> {
            if val_len < size_of::<T>() || ptr.is_null() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            Ok(unsafe { ptr.cast::<T>().read() })
        }

        match reg {
            x if x == IO_REGISTER_SOCKET_FLAGS => self.set_socket_flags(read_data(val, val_len)?),

            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }

    fn get_config(&self, reg: u32, val: *mut c_void, val_len: usize) -> Result<()> {
        let bytes = unsafe { std::slice::from_raw_parts_mut(val as *mut u8, val_len) };
        bytes.fill(0);

        fn write_data<T>(data: T, ptr: *mut c_void, val_len: usize) -> Result<()> {
            if val_len < size_of::<T>() || ptr.is_null() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            Ok(unsafe { ptr.cast::<T>().write(data) })
        }

        match reg {
            x if x == IO_REGISTER_ADDR => write_data(self.get_endpoint_addr(false)?, val, val_len),
            x if x == IO_REGISTER_PEER => write_data(self.get_endpoint_addr(true)?, val, val_len),
            x if x == IO_REGISTER_SOCKET_FLAGS => write_data(self.get_socket_flags(), val, val_len),

            _ => Ok(()),
        }
    }

    fn waitpoint(&self, kind: wait_kind) -> Result<(ThreadSyncSleep, bool)> {
        let sync = match self {
            SocketKind::TcpStream(smol_tcp_stream) => smol_tcp_stream
                .waitpoint(kind)
                .map_err(Into::into)
                .map(Into::into),
            SocketKind::TcpListener(smol_tcp_listener) => smol_tcp_listener
                .waitpoint(kind)
                .map_err(Into::into)
                .map(Into::into),
            SocketKind::UdpSocket(udp_socket) => udp_socket
                .get()
                .ok_or(TwzError::INVALID_ARGUMENT)?
                .waitpoint(kind)
                .map_err(Into::into)
                .map(Into::into),
        };
        let ready = self.is_ready(kind);
        sync.map(|s| (s, ready))
    }

    fn shutdown(&self, sh: std::net::Shutdown) -> Result<()> {
        match self {
            SocketKind::TcpStream(stream) => stream.shutdown(sh).map_err(Into::into),
            SocketKind::UdpSocket(stream) => stream
                .get()
                .ok_or(TwzError::INVALID_ARGUMENT)?
                .shutdown(sh)
                .map_err(Into::into),
            _ => Err(TwzError::NOT_SUPPORTED),
        }
    }

    fn as_socket(&self) -> Option<&SocketKind> {
        Some(self)
    }
}
