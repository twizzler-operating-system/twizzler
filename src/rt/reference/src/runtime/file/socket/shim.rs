use std::{
    io::{Error, ErrorKind, Read, Write},
    net::{Shutdown, SocketAddr, ToSocketAddrs},
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
};

use lazy_static::lazy_static;
use smoltcp::{
    iface::{Config, Context, Interface, SocketHandle, SocketSet},
    phy::{Device, Medium},
    socket::tcp::{ConnectError, ListenError, Socket, State},
    storage::RingBuffer,
    time::{Duration, Instant},
    wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
use virtio_net::{DeviceWrapper, TwizzlerTransport};

mod port;

use port::PortAssigner;
pub type SocketBuffer<'a> = RingBuffer<'a, u8>;
pub struct Engine {
    core: Arc<Mutex<Core>>,
    waiter: Arc<Condvar>,
    channel: mpsc::Sender<Option<(SocketHandle, u16)>>,
    _polling_thread: JoinHandle<()>,
}

struct Core {
    socketset: SocketSet<'static>,
    iface: Interface,
    device: DeviceWrapper<TwizzlerTransport>, // for now
}

const IP: &str = "10.0.2.15"; // QEMU user networking default IP
const GATEWAY: &str = "10.0.2.2"; // QEMU user networking gateway

const RX_BUF_SIZE: usize = 65536;
const TX_BUF_SIZE: usize = 8192;

lazy_static! {
    static ref ENGINE: Arc<Engine> = Arc::new(Engine::new());
    static ref PORTS: Arc<PortAssigner> = Arc::new(PortAssigner::new());
}

impl Engine {
    fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<Option<(SocketHandle, u16)>>();
        let (iface, device) = get_device_and_interface(sender.clone());
        let core = Arc::new(Mutex::new(Core::new(iface, device)));
        let waiter = Arc::new(Condvar::new());
        let _inner = core.clone();
        let _waiter = waiter.clone();

        // Okay, here is our background polling thread. It polls the network interface with the
        // SocketSet whenever it needs to, which is:
        // 1. when smoltcp says to based on poll_time() (calls poll_delay internally)
        // 2. when the state changes (eg a new socket is added)
        // 3. when blocking threads need to poll (we get a message on the channel)
        let thread = std::thread::spawn(move || {
            let inner = _inner;
            let waiter = _waiter;
            let mut tracking = Vec::new();

            fn check_tracking(tracking: &mut Vec<(SocketHandle, u16)>) {
                let mut core = ENGINE.core.lock().unwrap();
                let removed = tracking
                    .extract_if(0.., |item: &mut (SocketHandle, u16)| {
                        let socket = core.get_mutable_socket(item.0);
                        if socket.state() == State::Closed {
                            // log::debug!("tracked tcp socket {} in closed state", item.0);
                            core.release_socket(item.0);
                            true
                        } else {
                            false
                        }
                    })
                    .collect::<Vec<_>>();
                drop(core);
                for item in removed {
                    if item.1 != 0 {
                        PORTS.return_port(item.1);
                    }
                }
            }

            loop {
                check_tracking(&mut tracking);
                let time = {
                    let mut inner = inner.lock().unwrap();
                    inner.poll(&*waiter);
                    let time = inner.poll_time();

                    // We may need to poll immediately!
                    if matches!(time, Some(Duration::ZERO)) {
                        inner.poll(&*waiter);
                        continue;
                    }
                    time
                };

                // Wait until the designated timeout, or until we get a message on the channel.
                let inner = match time {
                    Some(dur) => receiver.recv_timeout(dur.into()).ok(),
                    None => receiver.recv().ok(),
                }
                .flatten();
                if let Some(inner) = inner {
                    // log::debug!("tracking socket {}, port {}", inner.0, inner.1);
                    tracking.push(inner);
                }
            }
        });
        Self {
            core,
            waiter,
            channel: sender,
            _polling_thread: thread,
        }
    }

    fn wake(&self) {
        let _ = self.channel.send(None);
    }

    fn add_socket(&self, socket: Socket<'static>) -> SocketHandle {
        self.core.lock().unwrap().add_socket(socket)
    }

    // Block until f returns Ok(R), and then return R. Note that f may be called multiple times,
    // and it may be called spuriously. If f returns Err(e) with e.kind() anything other than
    // NonBlock, return the error.
    fn blocking<R>(
        &self,
        mut f: impl FnMut(&mut Core) -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        let mut core = self.core.lock().unwrap();
        // Immediately poll, since we wait to have as up-to-date state as possible.
        core.poll(&self.waiter);
        // We'll need the polling thread to wake up and do work.
        self.wake();
        loop {
            match f(&mut *core) {
                Ok(r) => {
                    // We have done work, so again, notify the polling thread.
                    self.wake();
                    drop(core);
                    return Ok(r);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    core = self.waiter.wait(core).unwrap();
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn track(&self, inner: &TcpStreamInner) {
        let port = if inner.is_ephemeral_port {
            inner.port
        } else {
            0
        };
        let _ = self.channel.send(Some((inner.socket_handle, port)));
    }
}

impl Core {
    fn new(iface: Interface, device: DeviceWrapper<TwizzlerTransport>) -> Self {
        let socketset = SocketSet::new(Vec::new());
        Self {
            socketset,
            device,
            iface,
        }
    }

    fn add_socket(&mut self, sock: Socket<'static>) -> SocketHandle {
        self.socketset.add(sock)
    }

    fn get_socket(&mut self, handle: SocketHandle) -> &Socket<'static> {
        self.socketset.get(handle)
    }

    fn get_mutable_socket(&mut self, handle: SocketHandle) -> &mut Socket<'static> {
        self.socketset.get_mut(handle)
    }

    fn release_socket(&mut self, handle: SocketHandle) {
        self.socketset.remove(handle);
    }

    fn poll(&mut self, waiter: &Condvar) -> bool {
        let res = self
            .iface
            .poll(Instant::now(), &mut self.device, &mut self.socketset);
        // When we poll, notify the CV so that other waiting threads can retry their blocking
        // operations.
        waiter.notify_all();
        res
    }

    fn poll_time(&mut self) -> Option<Duration> {
        self.iface.poll_delay(Instant::now(), &mut self.socketset)
    }
}

// a variant of std's tcplistener using smoltcp's api
pub struct SmolTcpListener {
    listeners: Mutex<Vec<SocketHandle>>,
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
    ) -> Result<(u16, SocketAddr), ListenError> {
        let addrs = {
            match sock_addrs.to_socket_addrs() {
                Ok(addrs) => addrs,
                Err(_) => return Err(ListenError::InvalidState),
            }
        };
        for addr in addrs {
            match (*s).listen(addr.port()) {
                Ok(_) => return Ok((addr.port(), addr)),
                Err(_) => return Err(ListenError::Unaddressable),
            }
        }
        Err(ListenError::InvalidState)
    }

    fn do_bind<A: ToSocketAddrs>(addrs: A) -> Result<(Socket<'static>, u16, SocketAddr), Error> {
        let mut sock = {
            let rx_buffer = SocketBuffer::new(vec![0; RX_BUF_SIZE]);
            let tx_buffer = SocketBuffer::new(vec![0; TX_BUF_SIZE]);
            Socket::new(rx_buffer, tx_buffer) // this is the listening socket
        };
        let (port, local_address) = {
            match Self::each_addr(addrs, &mut sock) {
                Ok((port, local_address)) => (port, local_address),
                Err(_) => return Err(Error::other("listening error")),
            }
        };
        Ok((sock, port, local_address))
    }

    fn bind_once<A: ToSocketAddrs>(addrs: A) -> Result<Listener, Error> {
        let engine = &ENGINE;
        let (sock, port, local_address) = {
            match Self::do_bind(addrs) {
                Ok((sock, port, local_address)) => (sock, port, local_address),
                Err(_) => {
                    return Err(Error::other("listening error"));
                }
            }
        };
        let handle = (*engine).add_socket(sock);
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
    const BACKLOG: usize = 8;
    pub fn bind<A: ToSocketAddrs>(addrs: A) -> Result<SmolTcpListener, Error> {
        let mut listeners = Vec::with_capacity(Self::BACKLOG);
        let addr = std::cell::OnceCell::new();
        let port = std::cell::OnceCell::new();
        for _ in 0..Self::BACKLOG {
            match Self::bind_once(&addrs) {
                Ok(listener) => {
                    let _ = addr.set(listener.local_addr);
                    let _ = port.set(listener.port);
                    listeners.push(listener.socket_handle);
                }
                Err(_) => {
                    return Err(Error::other("listening error"));
                }
            }
        }
        let smoltcplistener = SmolTcpListener {
            listeners: Mutex::new(listeners),
            local_addr: *addr.get().ok_or(ErrorKind::AddrNotAvailable)?,
            port: *port.get().ok_or(ErrorKind::AddrNotAvailable)?,
        };
        // all listeners are now in the socket set and in the array within the SmolTcpListener
        Ok(smoltcplistener) // return the first listener
    }

    fn with_handle<R>(&self, listener_no: usize, f: impl FnOnce(&mut SocketHandle) -> R) -> R {
        let mut listeners = self.listeners.lock().unwrap();
        let handle = &mut listeners[listener_no];
        f(handle)
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
        // log::debug!("accept: {}:{}", self.local_addr, self.port);
        let engine = &ENGINE;
        let mut i: usize = 0;
        engine.blocking(|core| {
            loop {
                let stream = self.with_handle(i, |handle| {
                    let sock = core.get_mutable_socket(*handle);
                    if sock.is_active() {
                        let remote = sock.remote_endpoint().unwrap(); // the socket addr returned is that of the remote endpoint. ie. the client.
                        let remote_addr = SocketAddr::from((remote.addr, remote.port));
                        // creating another listener and swapping self's socket handle
                        let sock = {
                            match Self::do_bind(self.local_addr) {
                                Ok((sock, _, _)) => sock,
                                Err(_) => {
                                    return Err(Error::other("listening error"));
                                }
                            }
                        };
                        let newhandle = core.add_socket(sock);
                        let stream = SmolTcpStream {
                            inner: Arc::new(TcpStreamInner {
                                socket_handle: *handle,
                                port: self.port,
                                is_ephemeral_port: false,
                                rx_shutdown: AtomicBool::new(false),
                            }),
                        };
                        *handle = newhandle;
                        // log::debug!(
                        //     "accept: return for {}, socket {}",
                        //     remote_addr,
                        //     stream.inner.socket_handle
                        // );
                        Ok((stream, remote_addr))
                    } else {
                        Err(ErrorKind::WouldBlock.into())
                    }
                });
                match stream {
                    Ok(stream) => break Ok(stream),
                    Err(e) if e.kind() != ErrorKind::WouldBlock => {
                        break Err(e);
                    }
                    _ => {
                        i += 1;
                        if i == Self::BACKLOG {
                            i = 0;
                            break Err(ErrorKind::WouldBlock.into());
                        }
                    }
                }
            }
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        // rethink this one.
        // smoltcp supports fns listen_endpoint() and local_endpoint(). use one of those instead.
        return Ok(self.local_addr);
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

impl Read for SmolTcpStream {
    /* read():
     * parameters - reference to where the data should be placed upon reading
     * return - number of bytes read upon success; error upon error
     * loads the data read into the buffer given
     * if shutdown(Shutdown::Read) has been called, all reads will return Ok(0)
     */
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let engine = &ENGINE;
        engine.blocking(|core| {
            let socket = core.get_mutable_socket(self.inner.socket_handle);
            if socket.can_recv() {
                Ok(socket.recv_slice(buf).unwrap())
            } else if !socket.may_recv() || self.inner.rx_shutdown.load(Ordering::SeqCst) {
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                Ok(0)
            } else {
                Err(ErrorKind::WouldBlock.into())
            }
        })
    }
}

impl Write for SmolTcpStream {
    /* write():
     * parameters - reference to data to be written (represented as an array of u8)
     * result - number of bytes written upon success; error upon error.
     * writes given data to the connected socket.
     */
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
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
    fn flush(&mut self) -> Result<(), Error> {
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
        cx: &mut Context,
        port: u16,
    ) -> Result<(), ConnectError> {
        let addrs = {
            match sock_addrs.to_socket_addrs() {
                Ok(addrs) => addrs,
                Err(_) => return Err(ConnectError::InvalidState),
            }
        };
        for addr in addrs {
            match (*s).connect(cx, addr, port) {
                Ok(_) => return Ok(()),
                Err(_) => return Err(ConnectError::Unaddressable),
            }
        }
        Err(ConnectError::InvalidState) // is that the correct thing to return?
    }
    /* connect():
     * parameters: address(es) a list of addresses may be given. must take a REMOTE HOST'S
     * address return: a smoltcpstream that is connected to the remote server.
     */
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<SmolTcpStream, Error> {
        let engine = &ENGINE;
        let mut sock = {
            // create new socket
            let rx_buffer = SocketBuffer::new(vec![0; RX_BUF_SIZE]);
            let tx_buffer = SocketBuffer::new(vec![0; TX_BUF_SIZE]);
            Socket::new(rx_buffer, tx_buffer)
        };
        let ports = &PORTS;
        let Some(port) = ports.get_ephemeral_port() else {
            return Err(Error::other("dynamic port overflow!"));
        };
        let mut core = engine.core.lock().unwrap();
        if let Err(e) = Self::each_addr(addr, &mut sock, core.iface.context(), port) {
            ports.return_port(port);
            return Err(Error::other(format!("connection error: {e}")));
        }; // note to self: make sure remote endpoint matches the server address!
        let handle = engine.add_socket(sock);
        // log::debug!("connect: port {}, socket {}", port, handle);
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

    /* peer_addr():
     * parameters: -
     * return: the remote address of the socket. this is the address of the server
     * note: can only be used if already connected
     */
    pub fn peer_addr(&self) -> Result<SocketAddr, Error> {
        let engine = &ENGINE;
        let mut core = engine.core.lock().unwrap();
        let socket = core.get_socket(self.inner.socket_handle);
        let remote = socket.remote_endpoint().ok_or(ErrorKind::NotConnected)?;
        let remote_addr = SocketAddr::from((remote.addr, remote.port));
        Ok(remote_addr)
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
        // log::debug!(
        //     "socket {} shutdown: {:?}, state = {:?}",
        //     self.inner.socket_handle,
        //     how,
        //     socket.state()
        // );
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

    pub fn try_clone(&self) -> Result<SmolTcpStream, Error> {
        Ok(Self {
            inner: self.inner.clone(),
        })
    }
}

impl Drop for TcpStreamInner {
    fn drop(&mut self) {
        ENGINE.track(self);
    }
}

// implement impl std::fmt::Debug for SmolTcpStream
// add `#[derive(Debug)]` to `SmolTcpStream` or manually `impl std::fmt::Debug for SmolTcpStream`

fn get_device_and_interface(
    notifier: std::sync::mpsc::Sender<Option<(SocketHandle, u16)>>,
) -> (Interface, DeviceWrapper<TwizzlerTransport>) {
    use virtio_net::get_device;
    let mut device = get_device(notifier);

    if device.capabilities().medium != Medium::Ethernet {
        panic!("This implementation only supports virtio-net which is an ethernet device");
    }

    let hardware_addr = HardwareAddress::Ethernet(device.mac_address());

    // Create interface
    let mut config = Config::new(hardware_addr);
    config.random_seed = 0x2333;

    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::from_str(IP).unwrap(), 24))
            .unwrap();
    });

    iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::from_str(GATEWAY).unwrap())
        .unwrap();
    (iface, device)
}
