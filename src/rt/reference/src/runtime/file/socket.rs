mod engine;
mod smoltcp;

use std::{
    net::{SocketAddr, ToSocketAddrs},
    os::raw::c_void,
    sync::{atomic::AtomicU64, Arc},
};

use secgate::TwzError;
pub use smoltcp::{dns, SmolTcpListener, SmolTcpStream};
use twizzler_rt_abi::{
    bindings::{wait_kind, IO_REGISTER_ADDR, IO_REGISTER_PEER, IO_REGISTER_SOCKET_FLAGS},
    fd::SocketAddress,
    io::IoFlags,
};

use crate::runtime::file::socket::smoltcp::UdpSocket;

#[derive(Clone)]
pub enum SocketKind {
    None,
    TcpStream(Arc<SmolTcpStream>),
    TcpListener(Arc<SmolTcpListener>),
    UdpSocket(Arc<UdpSocket>),
}

impl SocketKind {
    fn get_endpoint_addr(&self, peer: bool) -> Result<SocketAddress, TwzError> {
        match self {
            SocketKind::None => Err(TwzError::INVALID_ARGUMENT),
            SocketKind::TcpStream(smol_tcp_stream) => Ok(smol_tcp_stream.addr(peer)),
            SocketKind::TcpListener(smol_tcp_listener) => Ok(smol_tcp_listener.addr(peer)),
            SocketKind::UdpSocket(udp_socket) => Ok(udp_socket.addr(peer)),
        }
    }

    fn get_socket_flags(&self) -> Result<u32, TwzError> {
        match self {
            SocketKind::None => Err(TwzError::INVALID_ARGUMENT),
            SocketKind::TcpStream(smol_tcp_stream) => Ok(smol_tcp_stream.flags()),
            SocketKind::TcpListener(smol_tcp_listener) => Ok(smol_tcp_listener.flags()),
            SocketKind::UdpSocket(udp_socket) => Ok(udp_socket.flags()),
        }
    }

    fn set_socket_flags(&self, flags: u32) -> Result<(), TwzError> {
        match self {
            SocketKind::None => Err(TwzError::INVALID_ARGUMENT),
            SocketKind::TcpStream(smol_tcp_stream) => Ok(smol_tcp_stream.set_flags(flags)),
            SocketKind::TcpListener(smol_tcp_listener) => Ok(smol_tcp_listener.set_flags(flags)),
            SocketKind::UdpSocket(udp_socket) => Ok(udp_socket.set_flags(flags)),
        }
    }

    pub fn set_config(&self, reg: u32, val: *const c_void, val_len: usize) -> Result<(), TwzError> {
        fn read_data<T>(ptr: *const c_void, val_len: usize) -> Result<T, TwzError> {
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

    pub fn get_config(&self, reg: u32, val: *mut c_void, val_len: usize) -> Result<(), TwzError> {
        let bytes = unsafe { std::slice::from_raw_parts_mut(val as *mut u8, val_len) };
        bytes.fill(0);

        fn write_data<T>(data: T, ptr: *mut c_void, val_len: usize) -> Result<(), TwzError> {
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

    pub fn waitpoint(&self, kind: wait_kind) -> Result<(*const AtomicU64, u64), TwzError> {
        match self {
            SocketKind::None => Err(TwzError::NOT_SUPPORTED),
            SocketKind::TcpStream(smol_tcp_stream) => smol_tcp_stream.waitpoint(kind),
            SocketKind::TcpListener(smol_tcp_listener) => smol_tcp_listener.waitpoint(kind),
            SocketKind::UdpSocket(udp_socket) => udp_socket.waitpoint(kind),
        }
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        SmolTcpListener::bind(addr).map(|listener| SocketKind::TcpListener(Arc::new(listener)))
    }

    pub fn udp_bind<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        UdpSocket::bind(addr).map(|listener| SocketKind::UdpSocket(Arc::new(listener)))
    }

    pub fn accept(&self) -> Result<Self, std::io::Error> {
        match self {
            SocketKind::TcpListener(listener) => listener
                .accept(IoFlags::empty())
                .map(|stream| SocketKind::TcpStream(Arc::new(stream.0))),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid socket kind",
            )),
        }
    }

    pub fn udp_connect<A: ToSocketAddrs>(&self, addr: A) -> Result<(), std::io::Error> {
        match self {
            SocketKind::UdpSocket(udp_socket) => udp_socket.connect(addr),
            _ => panic!("invalid socket type"),
        }
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        SmolTcpStream::connect(IoFlags::empty(), addr)
            .map(|stream| SocketKind::TcpStream(Arc::new(stream)))
    }

    pub fn close(&self) -> Result<(), std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.shutdown(std::net::Shutdown::Both),
            SocketKind::UdpSocket(stream) => stream.shutdown(std::net::Shutdown::Both),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }

    pub fn shutdown(&self, shutdown: std::net::Shutdown) -> Result<(), std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.shutdown(shutdown),
            SocketKind::UdpSocket(stream) => stream.shutdown(shutdown),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }
}

impl SocketKind {
    pub fn read(&self, buf: &mut [u8], flags: IoFlags) -> Result<usize, std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.read(buf, flags),
            SocketKind::UdpSocket(stream) => stream.read(buf, flags),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }

    pub fn read_from(
        &self,
        buf: &mut [u8],
        ep: &mut twizzler_rt_abi::io::Endpoint,
        flags: IoFlags,
    ) -> Result<usize, std::io::Error> {
        match self {
            SocketKind::UdpSocket(stream) => {
                let val = stream.read_from(buf, flags)?;
                if let Some(addr) = val.1.map(|x| x.endpoint) {
                    let sa = SocketAddr::from((addr.addr, addr.port));
                    let sa = twizzler_rt_abi::fd::SocketAddress::from(sa);
                    *ep = sa.into();
                }
                Ok(val.0)
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }

    pub fn write_to(
        &self,
        buf: &[u8],
        ep: &twizzler_rt_abi::io::Endpoint,
        flags: IoFlags,
    ) -> Result<usize, std::io::Error> {
        let sa = twizzler_rt_abi::fd::SocketAddress::try_from(*ep)?;
        let sa = SocketAddr::from(sa);
        match self {
            SocketKind::UdpSocket(stream) => {
                stream.write_to(buf, sa.into(), flags)?;
                Ok(buf.len())
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }
}

impl SocketKind {
    pub fn write(&self, buf: &[u8], flags: IoFlags) -> Result<usize, std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.write(buf, flags),
            SocketKind::UdpSocket(stream) => stream.write(buf, flags).map(|_| buf.len()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }

    pub fn flush(&self) -> Result<(), std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.flush(),
            SocketKind::UdpSocket(stream) => stream.flush(),
            _ => Ok(()),
        }
    }
}
