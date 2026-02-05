use std::{
    io::{Error, ErrorKind},
    net::{Shutdown, SocketAddr, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use smoltcp::{
    iface::SocketHandle,
    socket::tcp::{Socket, State},
    storage::RingBuffer,
};

mod engine;
mod port;

use engine::ENGINE;

use crate::runtime::file::socket::shim::engine::PORTS;

pub type SocketBuffer<'a> = RingBuffer<'a, u8>;

const IP: &str = "10.0.2.15"; // QEMU user networking default IP
const _GATEWAY: &str = "10.0.2.2"; // QEMU user networking gateway

const RX_BUF_SIZE: usize = 65536 * 2;
const TX_BUF_SIZE: usize = 8192 * 2;

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

        eprintln!("binding...");
        for _ in 0..Self::BACKLOG {
            let listener = Self::bind_once(&addrs)?;
            listeners.push(listener);
        }
        eprintln!("binding done");

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
                eprintln!("got: {}", socket.recv_queue());
                Ok(socket.recv_slice(buf).unwrap())
            } else if (!socket.may_recv() || self.inner.rx_shutdown.load(Ordering::SeqCst))
                && socket.state() != State::SynReceived
                && socket.state() != State::SynSent
            {
                eprintln!(
                    "b: {} {} {:?}",
                    socket.may_recv(),
                    self.inner.rx_shutdown.load(Ordering::SeqCst),
                    socket.state()
                );
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                Ok(0)
            } else {
                eprintln!("would block");
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
        ENGINE.track(self);
    }
}
