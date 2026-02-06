use std::{
    io::{Error, ErrorKind},
    net::{Shutdown, SocketAddr, ToSocketAddrs},
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use smoltcp::{
    iface::SocketHandle,
    socket::{
        dns::GetQueryResultError,
        tcp::{Socket, State},
        udp::{Socket as SmolUdpSocket, UdpMetadata},
    },
    storage::{PacketBuffer, PacketMetadata, RingBuffer},
    wire::{IpAddress, IpEndpoint},
};

mod engine;
mod port;

use engine::ENGINE;
use smoltcp::socket::dns::Socket as DnsSocket;

use crate::runtime::file::socket::shim::engine::PORTS;

pub type SocketBuffer<'a> = RingBuffer<'a, u8>;

const RX_BUF_SIZE: usize = 65536;
const TX_BUF_SIZE: usize = 8192;

// a variant of std's tcplistener using smoltcp's api
pub struct SmolTcpListener {
    listeners: Mutex<Vec<Listener>>,
    local_addr: SocketAddr,
    port: u16,
}

struct Listener {
    socket_handle: SocketHandle,
    local_addr: SocketAddr,
    port: u16,
}

impl SmolTcpListener {
    /* each_addr():
     * parameters:
     * helper function for bind()
     * processes each address given to see whether it can implement ToSocketAddr, then tries to
     * listen on that addr keeps trying each address until one of them successfully listens
     */
    fn each_addr<A: ToSocketAddrs>(
        sock_addrs: A,
        s: &mut Socket<'static>,
    ) -> Result<(u16, SocketAddr), Error> {
        let addrs = sock_addrs.to_socket_addrs()?;
        for addr in addrs {
            tracing::debug!("each_addr: {:?}", addr);
            match s.listen(addr.port()) {
                Ok(_) => return Ok((addr.port(), addr)),
                Err(_) => {}
            }
        }
        Err(Error::new(
            ErrorKind::AddrNotAvailable,
            "failed to listen on any address",
        ))
    }

    fn do_bind<A: ToSocketAddrs>(addrs: A) -> Result<(Socket<'static>, u16, SocketAddr), Error> {
        let mut sock = {
            let rx_buffer = SocketBuffer::new(vec![0; RX_BUF_SIZE]);
            let tx_buffer = SocketBuffer::new(vec![0; TX_BUF_SIZE]);
            Socket::new(rx_buffer, tx_buffer) // this is the listening socket
        };
        let (port, local_address) = Self::each_addr(addrs, &mut sock)?;
        Ok((sock, port, local_address))
    }

    fn bind_once<A: ToSocketAddrs>(addrs: A) -> Result<Listener, Error> {
        let (sock, port, local_address) =
            Self::do_bind(addrs).inspect_err(|e| tracing::warn!("do_bind: {e}"))?;
        let handle = ENGINE.add_socket(sock);
        let tcp_listener = Listener {
            socket_handle: handle,
            port,
            local_addr: local_address,
        };
        Ok(tcp_listener)
    }
    /* bind
     * accepts: address(es)
     * returns: a tcpsocket
     * creates a tcpsocket and binds the address to that socket.
     * if multiple addresses given, it will attempt to bind to each until successful

        example arguments passed to bind:
        "127.0.0.1:0"
        SocketAddr::from(([127, 0, 0, 1], 443))
        let addrs = [ SocketAddr::from(([127, 0, 0, 1], 80)),  SocketAddr::from(([127, 0, 0, 1], 443)), ];
    */
    const BACKLOG: usize = 64;
    pub fn bind<A: ToSocketAddrs>(addrs: A) -> Result<SmolTcpListener, Error> {
        let mut listeners = Vec::with_capacity(Self::BACKLOG);

        for _ in 0..Self::BACKLOG {
            let listener = Self::bind_once(&addrs)?;
            listeners.push(listener);
        }

        let smoltcplistener = SmolTcpListener {
            local_addr: listeners[0].local_addr,
            port: listeners[0].port,
            listeners: Mutex::new(listeners),
        };
        // all listeners are now in the socket set and in the array within the SmolTcpListener
        Ok(smoltcplistener) // return the first listener
    }

    // accept
    // create a new socket for tcpstream
    // ^^ creating a new one so that the user can call accept() on the previous one again
    // return tcpstream
    /* accept():
     * parameters: -
     * return: (SmolTcpStream, SocketAddr) upon success; Error upon failure
     * takes the current listener and advances the socket's state (in terms of the smoltcp state
     * machine)
     */
    // to think about: each socket must be pulled from the engine and checked for activeness.
    pub fn accept(&self) -> Result<(SmolTcpStream, SocketAddr), Error> {
        tracing::debug!("accept: {}", self.local_addr);
        ENGINE.blocking(|core| {
            let mut listeners = self.listeners.lock().unwrap();

            for listener in &mut *listeners {
                let sock = core.get_mutable_socket(listener.socket_handle);
                if sock.is_active() {
                    let remote = sock.remote_endpoint().unwrap(); // the socket addr returned is that of the remote endpoint. ie. the client.
                    let remote_addr = SocketAddr::from((remote.addr, remote.port));
                    // creating another listener and swapping self's socket handle
                    let sock = Self::do_bind(self.local_addr).inspect_err(|e| {
                        tracing::warn!("failed to rebind new socket after accept: {e}")
                    })?;
                    let newhandle = core.add_socket(sock.0);

                    let stream = SmolTcpStream {
                        inner: Arc::new(TcpStreamInner {
                            socket_handle: listener.socket_handle,
                            port: self.port,
                            is_ephemeral_port: false,
                            rx_shutdown: AtomicBool::new(false),
                        }),
                    };
                    listener.socket_handle = newhandle;
                    return Ok((stream, remote_addr));
                } else if !sock.is_open() {
                    // Connection was reset?
                    if let Ok(sock) = Self::do_bind(self.local_addr).inspect_err(|e| {
                        tracing::warn!("failed to rebind socket after detecting reset: {e}")
                    }) {
                        let newhandle = core.add_socket(sock.0);
                        listener.socket_handle = newhandle;
                    }
                }
            }
            Err(Error::from(ErrorKind::WouldBlock))
        })
    }
}

struct TcpStreamInner {
    socket_handle: SocketHandle,
    port: u16,
    is_ephemeral_port: bool,
    rx_shutdown: AtomicBool,
}

pub struct SmolTcpStream {
    inner: Arc<TcpStreamInner>,
}

impl core::fmt::Debug for SmolTcpStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmolTcpStream")
            .field("socket_handle", &self.inner.socket_handle)
            .field("port", &self.inner.port)
            .finish_non_exhaustive()
    }
}

impl SmolTcpStream {
    /* read():
     * parameters - reference to where the data should be placed upon reading
     * return - number of bytes read upon success; error upon error
     * loads the data read into the buffer given
     * if shutdown(Shutdown::Read) has been called, all reads will return Ok(0)
     */
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        let engine = &ENGINE;
        engine.blocking(|core| {
            let socket = core.get_mutable_socket(self.inner.socket_handle);
            if socket.can_recv() {
                Ok(socket.recv_slice(buf).unwrap())
            } else if (!socket.may_recv() || self.inner.rx_shutdown.load(Ordering::SeqCst))
                && socket.state() != State::SynReceived
                && socket.state() != State::SynSent
            {
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                Ok(0)
            } else {
                Err(ErrorKind::WouldBlock.into())
            }
        })
    }
}

impl SmolTcpStream {
    /* write():
     * parameters - reference to data to be written (represented as an array of u8)
     * result - number of bytes written upon success; error upon error.
     * writes given data to the connected socket.
     */
    pub fn write(&self, buf: &[u8]) -> Result<usize, Error> {
        let engine = &ENGINE;
        engine.blocking(|core| {
            let socket = core.get_mutable_socket(self.inner.socket_handle);
            if socket.can_send() {
                Ok(socket.send_slice(buf).unwrap())
            } else if !socket.may_send() {
                Err(ErrorKind::ConnectionReset.into())
            } else {
                Err(ErrorKind::WouldBlock.into())
            }
        })
    }
    /* flush():
     */
    pub fn flush(&self) -> Result<(), Error> {
        Ok(())
        // lol this is what std::net::TcpStream::flush() does:
        // https://doc.rust-lang.org/src/std/net/tcp.rs.html#695
        // also smoltcp doesn't have a flush method for its socket buffer
    }
}

impl SmolTcpStream {
    /* each_addr:
     * helper function for connect()
     * processes each address given to see whether it can implement ToSocketAddr, then tries to
     * connect to that addr keeps trying each address until one of them successfully connects
     * parameters: addresses passed into connect(), reference to socket, reference to
     * interface context, and port.
     * return: port and address
     */
    fn each_addr<A: ToSocketAddrs>(
        sock_addrs: A,
        s: &mut Socket<'static>,
        port: u16,
    ) -> Result<(), Error> {
        let addrs = sock_addrs.to_socket_addrs()?;
        for addr in addrs {
            if let Some(res) =
                ENGINE.with_iface_for(addr, |iface| match s.connect(iface.context(), addr, port) {
                    Ok(_) => return Ok(()),
                    Err(_) => return Err(ErrorKind::AddrNotAvailable.into()),
                })
            {
                return res;
            }
        }
        Err(ErrorKind::AddrNotAvailable.into()) // is that the correct thing to return?
    }
    /* connect():
     * parameters: address(es) a list of addresses may be given. must take a REMOTE HOST'S
     * address return: a smoltcpstream that is connected to the remote server.
     */
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<SmolTcpStream, Error> {
        let mut sock = {
            // create new socket
            let rx_buffer = SocketBuffer::new(vec![0; RX_BUF_SIZE]);
            let tx_buffer = SocketBuffer::new(vec![0; TX_BUF_SIZE]);
            Socket::new(rx_buffer, tx_buffer)
        };
        let Some(port) = PORTS.get_ephemeral_port() else {
            return Err(Error::other("dynamic port overflow!"));
        };
        tracing::debug!("connect: {}", port);
        if let Err(e) = Self::each_addr(addr, &mut sock, port) {
            PORTS.return_port(port);
            return Err(e);
        };
        let handle = ENGINE.add_socket(sock);

        ENGINE.blocking(|core| {
            let socket = core.get_mutable_socket(handle);
            if socket.may_send() || socket.may_recv() {
                Ok(())
            } else if !socket.is_active() {
                tracing::error!("connection reset! ({:?})", socket.state());
                return Err(ErrorKind::ConnectionReset.into());
            } else {
                Err(ErrorKind::WouldBlock.into())
            }
        })?;

        let smoltcpstream = SmolTcpStream {
            inner: Arc::new(TcpStreamInner {
                socket_handle: handle,
                port,
                rx_shutdown: AtomicBool::new(false),
                is_ephemeral_port: true,
            }),
        };
        Ok(smoltcpstream)
    }

    /* shutdown():
     * parameters: how - an enum of Shutdown that specifies what part of the socket to shutdown.
     *             options are Read, Write, or Both.
     * return: Result<> indicating success, (), or failure, Error
     */
    /*
    "Calling this function multiple times may result in different behavior,
    depending on the operating system. On Linux, the second call will
    return `Ok(())`, but on macOS, it will return `ErrorKind::NotConnected`.
    This may change in the future." -- std::net documentation
    // Twizzler returns Ok(())
    */
    pub fn shutdown(&self, how: Shutdown) -> Result<(), Error> {
        // specifies shutdown of read, write, or both with enum Shutdown
        let engine = &ENGINE;
        let mut core = engine.core.lock().unwrap(); // acquire mutex
        let socket = core.get_mutable_socket(self.inner.socket_handle);
        tracing::debug!(
            "socket {} shutdown: {:?}, state = {:?}",
            self.inner.socket_handle,
            how,
            socket.state()
        );
        if socket.state() == State::Closed {
            // if already closed, exit early
            return Ok(());
        }
        match how {
            Shutdown::Read => {
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                return Ok(());
            }
            Shutdown::Write => {
                socket.close();
                return Ok(());
            }
            Shutdown::Both => {
                socket.close();
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                return Ok(());
            }
        }
    }
}

impl Drop for TcpStreamInner {
    fn drop(&mut self) {
        ENGINE.track(self.socket_handle, self.port, self.is_ephemeral_port);
    }
}

struct UdpSocketInner {
    socket_handle: SocketHandle,
    port: u16,
    is_ephemeral_port: bool,
    rx_shutdown: AtomicBool,
    connect_addr: Mutex<Option<IpEndpoint>>,
}

pub struct UdpSocket {
    inner: Arc<UdpSocketInner>,
}

impl core::fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmolTcpStream")
            .field("socket_handle", &self.inner.socket_handle)
            .field("port", &self.inner.port)
            .finish_non_exhaustive()
    }
}

impl UdpSocket {
    pub fn read_from(&self, buf: &mut [u8]) -> Result<(usize, Option<UdpMetadata>), Error> {
        let engine = &ENGINE;
        engine.blocking(|core| {
            let socket = core.get_mutable_udp_socket(self.inner.socket_handle);
            if socket.can_recv() {
                Ok(socket.recv_slice(buf).map(|x| (x.0, Some(x.1))).unwrap())
            } else if !socket.is_open() || self.inner.rx_shutdown.load(Ordering::SeqCst) {
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                Ok((0, None))
            } else {
                Err(ErrorKind::WouldBlock.into())
            }
        })
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        self.read_from(buf).map(|x| x.0)
    }
}

impl UdpSocket {
    /* write():
     * parameters - reference to data to be written (represented as an array of u8)
     * result - number of bytes written upon success; error upon error.
     * writes given data to the connected socket.
     */
    pub fn write_to(&self, buf: &[u8], meta: UdpMetadata) -> Result<(), Error> {
        ENGINE.blocking(|core| {
            let socket = core.get_mutable_udp_socket(self.inner.socket_handle);
            if socket.can_send() {
                Ok(socket.send_slice(buf, meta).unwrap())
            } else if !socket.is_open() {
                Err(ErrorKind::ConnectionReset.into())
            } else {
                Err(ErrorKind::WouldBlock.into())
            }
        })
    }

    pub fn write(&self, buf: &[u8]) -> Result<(), Error> {
        let target = *self.inner.connect_addr.lock().unwrap();
        let meta = match target {
            Some(addr) => UdpMetadata::from(addr),
            None => return Err(ErrorKind::NotConnected.into()),
        };
        self.write_to(buf, meta)
    }

    pub fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

impl UdpSocket {
    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> Result<(), Error> {
        *self.inner.connect_addr.lock().unwrap() =
            Some(addr.to_socket_addrs()?.next().unwrap().into());
        Ok(())
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, Error> {
        let mut sock = {
            SmolUdpSocket::new(
                PacketBuffer::new(vec![PacketMetadata::EMPTY; 1024], vec![0; RX_BUF_SIZE]),
                PacketBuffer::new(vec![PacketMetadata::EMPTY; 1024], vec![0; TX_BUF_SIZE]),
            )
        };
        let mut ephem = false;
        for addr in addr.to_socket_addrs()? {
            let (addr, mut port) = (addr.ip(), addr.port());
            ephem = port == 0;
            if ephem {
                port = PORTS.get_ephemeral_port().ok_or(ErrorKind::ResourceBusy)?;
            }
            if sock.bind((addr, port)).is_ok() {
                break;
            }
            if ephem {
                PORTS.return_port(port);
            }
        }
        if !sock.endpoint().is_specified() {
            return Err(Error::new(
                ErrorKind::AddrNotAvailable,
                "address not available",
            ));
        }
        let port = sock.endpoint().port;
        let socket_handle = ENGINE.add_udp_socket(sock);
        Ok(Self {
            inner: Arc::new(UdpSocketInner {
                socket_handle,
                port,
                is_ephemeral_port: ephem,
                rx_shutdown: AtomicBool::new(false),
                connect_addr: Mutex::new(None),
            }),
        })
    }

    pub fn shutdown(&self, how: Shutdown) -> Result<(), Error> {
        // specifies shutdown of read, write, or both with enum Shutdown
        let engine = &ENGINE;
        let mut core = engine.core.lock().unwrap(); // acquire mutex
        let socket = core.get_mutable_udp_socket(self.inner.socket_handle);
        tracing::debug!("socket {} shutdown: {:?}", self.inner.socket_handle, how,);
        if !socket.is_open() {
            // if already closed, exit early
            return Ok(());
        }
        match how {
            Shutdown::Read => {
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                return Ok(());
            }
            Shutdown::Write => {
                socket.close();
                return Ok(());
            }
            Shutdown::Both => {
                socket.close();
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                return Ok(());
            }
        }
    }
}

impl Drop for UdpSocketInner {
    fn drop(&mut self) {
        ENGINE.track(self.socket_handle, self.port, self.is_ephemeral_port);
    }
}

pub fn dns(query: &str) -> Result<Vec<SocketAddr>, Error> {
    let (name, port) = query.rsplit_once(":").ok_or(ErrorKind::InvalidInput)?;
    let port = port.parse::<u16>().map_err(|_| ErrorKind::InvalidInput)?;
    let mut q = vec![];
    q.extend((0..16).map(|_| None));
    let mut socket = DnsSocket::new(
        &[IpAddress::from_str("8.8.8.8").map_err(|_| ErrorKind::InvalidInput)?],
        q,
    );
    let mut core = ENGINE.core.lock().unwrap();
    let iface = core.iface_for_dns().ok_or(ErrorKind::Unsupported)?;
    let qh = match socket.start_query(iface.context(), name, smoltcp::wire::DnsQueryType::A) {
        Ok(qh) => qh,
        Err(e) => match e {
            smoltcp::socket::dns::StartQueryError::NoFreeSlot => Err(ErrorKind::OutOfMemory),
            _ => Err(ErrorKind::InvalidInput),
        }?,
    };
    let handle = core.add_dns_socket(socket);
    drop(core);

    let res = ENGINE.blocking(|core| {
        let socket = core.get_mutable_dns_socket(handle);
        let res = socket.get_query_result(qh);
        match res {
            Err(GetQueryResultError::Pending) => Err(ErrorKind::WouldBlock.into()),
            Err(GetQueryResultError::Failed) => Err(ErrorKind::AddrNotAvailable.into()),
            Ok(v) => Ok(v),
        }
    });
    let mut core = ENGINE.core.lock().unwrap();
    core.release_socket(handle);
    drop(core);
    let res = res?;
    Ok(res.into_iter().map(|a| (a, port).into()).collect())
}
