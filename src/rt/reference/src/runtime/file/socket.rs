mod shim;

use std::{net::ToSocketAddrs, sync::Arc};

use shim::{SmolTcpListener, SmolTcpStream};

#[derive(Clone)]
pub enum SocketKind {
    TcpStream(Arc<SmolTcpStream>),
    TcpListener(Arc<SmolTcpListener>),
}

impl SocketKind {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        SmolTcpListener::bind(addr).map(|listener| SocketKind::TcpListener(Arc::new(listener)))
    }

    pub fn accept(&self) -> Result<Self, std::io::Error> {
        match self {
            SocketKind::TcpListener(listener) => listener
                .accept()
                .map(|stream| SocketKind::TcpStream(Arc::new(stream.0))),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid socket kind",
            )),
        }
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        SmolTcpStream::connect(addr).map(|stream| SocketKind::TcpStream(Arc::new(stream)))
    }

    pub fn close(&self) -> Result<(), std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.shutdown(std::net::Shutdown::Both),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }
}

impl SocketKind {
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.read(buf),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }
}

impl SocketKind {
    pub fn write(&self, buf: &[u8]) -> Result<usize, std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.write(buf),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }

    pub fn flush(&self) -> Result<(), std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.flush(),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Invalid socket kind",
            )),
        }
    }
}
