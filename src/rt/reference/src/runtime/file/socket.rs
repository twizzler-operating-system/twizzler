mod shim;

use std::net::{ToSocketAddrs};
use std::io::{Read, Write};
use shim::{SmolTcpStream, SmolTcpListener};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub enum SocketKind {
    TcpStream(Arc<Mutex<TcpStream>>),
    TcpListener(Arc<Mutex<TcpListener>>),
}

pub struct TcpStream(SmolTcpStream);

pub struct TcpListener(SmolTcpListener);


impl SocketKind {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        SmolTcpListener::bind(addr).map(|listener| SocketKind::TcpListener(Arc::new(Mutex::new(TcpListener(listener)))))
    }

    pub fn accept(&self) -> Result<Self, std::io::Error> {
    match self {
        SocketKind::TcpListener(listener) => {
            listener.lock().unwrap()
                .accept()
                .map(|stream| SocketKind::TcpStream(Arc::new(Mutex::new(stream))))
        }
        _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid socket kind")),
    }
}

    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        SmolTcpStream::connect(addr).map(|stream| SocketKind::TcpStream(Arc::new(Mutex::new(TcpStream(stream)))))
    }

    pub fn close(&self) -> Result<(), std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.lock().unwrap().close(),
            _ => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Invalid socket kind")),
        }
    }
}

impl Read for SocketKind {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.lock().unwrap().read(buf),
            _ => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Invalid socket kind")),
        }
    }
}

impl Write for SocketKind {
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.lock().unwrap().write(buf),
            _ => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Invalid socket kind")),
        }
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        match self {
            SocketKind::TcpStream(stream) => stream.lock().unwrap().flush(),
            _ => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Invalid socket kind")),
        }
    }
}

impl TcpStream {
    fn connect<A: ToSocketAddrs>(addr: A) -> Result<TcpStream, std::io::Error> {
        SmolTcpStream::connect(addr).map(TcpStream)
    }

    fn close(&self) -> Result<(), std::io::Error> {
        self.0.shutdown(std::net::Shutdown::Both)
    }
}

impl Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        self.0.read(buf)
    }
}

impl Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.0.flush()
    }
}

impl TcpListener {
    fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        SmolTcpListener::bind(addr).map(Self)
    }

    fn accept(&self) -> Result<TcpStream, std::io::Error> {
        self.0.accept().map(|(stream, _)| TcpStream(stream))
    }
}