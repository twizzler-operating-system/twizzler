use std::{
    io::{Error, ErrorKind},
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
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, Loopback, Medium},
    socket::tcp::{Socket, State},
    storage::RingBuffer,
    time::{Duration, Instant},
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address},
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
    ifaceset: Vec<IfaceSet>,
}

enum SupportedDevices {
    Lo(Loopback),
    Twz(DeviceWrapper<TwizzlerTransport>),
}

struct IfaceSet {
    ifaces: Vec<Interface>,
    device: SupportedDevices,
}

impl IfaceSet {
    fn new(device: SupportedDevices) -> Self {
        let ifaces = Vec::new();
        Self { ifaces, device }
    }

    fn insert_iface(&mut self, iface: Interface) {
        self.ifaces.push(iface);
    }

    fn poll(&mut self, socketset: &mut SocketSet<'static>) -> bool {
        let mut ready = false;
        for iface in &mut self.ifaces {
            match self.device {
                SupportedDevices::Lo(ref mut lo) => {
                    ready |= iface.poll(Instant::now(), lo, socketset);
                }
                SupportedDevices::Twz(ref mut twz) => {
                    ready |= iface.poll(Instant::now(), twz, socketset);
                }
            }
        }
        ready
    }

    fn poll_time(&mut self, socketset: &mut SocketSet<'static>) -> Option<Duration> {
        let mut min_delay = None;
        for iface in &mut self.ifaces {
            if let Some(delay) = iface.poll_delay(Instant::now(), socketset) {
                min_delay = Some(min_delay.map_or(delay, |min: Duration| min.min(delay)));
            }
        }
        min_delay
    }

    fn find_iface_for(&mut self, _addr: SocketAddr) -> Option<&mut Interface> {
        // TODO
        self.ifaces.get_mut(0)
    }
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
        let (lo_iface, lo_device) = get_lo_device_and_interface(sender.clone());

        let mut nic = IfaceSet::new(SupportedDevices::Twz(device));
        nic.insert_iface(iface);

        let mut lo = IfaceSet::new(SupportedDevices::Lo(lo_device));
        lo.insert_iface(lo_iface);

        let core = Arc::new(Mutex::new(Core::new(vec![lo])));
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
                            tracing::info!("tracked tcp socket {} in closed state", item.0);
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
                    if time.is_some_and(|time| time.total_micros() < 100) {
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
                    tracing::info!("tracking socket {}, port {}", inner.0, inner.1);
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
    fn new(ifaceset: Vec<IfaceSet>) -> Self {
        let socketset = SocketSet::new(Vec::new());
        Self {
            socketset,
            ifaceset,
        }
    }

    fn add_socket(&mut self, sock: Socket<'static>) -> SocketHandle {
        self.socketset.add(sock)
    }

    fn get_mutable_socket(&mut self, handle: SocketHandle) -> &mut Socket<'static> {
        self.socketset.get_mut(handle)
    }

    fn release_socket(&mut self, handle: SocketHandle) {
        self.socketset.remove(handle);
    }

    fn poll(&mut self, waiter: &Condvar) -> bool {
        let mut res = false;
        for ifaceset in &mut self.ifaceset {
            res |= ifaceset.poll(&mut self.socketset);
        }
        // When we poll, notify the CV so that other waiting threads can retry their blocking
        // operations.
        waiter.notify_all();
        res
    }

    fn poll_time(&mut self) -> Option<Duration> {
        let mut min_time = None;
        for ifaceset in &mut self.ifaceset {
            if let Some(time) = ifaceset.poll_time(&mut self.socketset) {
                min_time = Some(min_time.map_or(time, |t: Duration| t.min(time)));
            }
        }
        min_time
    }

    fn find_iface_for(&mut self, addr: SocketAddr) -> Option<&mut Interface> {
        for ifaceset in &mut self.ifaceset {
            if let Some(iface) = ifaceset.find_iface_for(addr) {
                return Some(iface);
            }
        }
        None
    }
}

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
            tracing::info!("each_addr: {:?}", addr);
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
    const BACKLOG: usize = 8;
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

    fn with_handle<R>(&self, listener_no: usize, f: impl FnOnce(&mut Listener) -> R) -> R {
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
        tracing::info!("accept: {}", self.local_addr);
        let engine = &ENGINE;
        let mut i: usize = 0;
        engine.blocking(|core| {
            loop {
                let stream = self.with_handle(i, |handle| {
                    let sock = core.get_mutable_socket(handle.socket_handle);
                    if sock.is_active() {
                        let remote = sock.remote_endpoint().unwrap(); // the socket addr returned is that of the remote endpoint. ie. the client.
                        let remote_addr = SocketAddr::from((remote.addr, remote.port));
                        // creating another listener and swapping self's socket handle
                        let sock = Self::do_bind(self.local_addr)?;
                        let newhandle = core.add_socket(sock.0);

                        let stream = SmolTcpStream {
                            inner: Arc::new(TcpStreamInner {
                                socket_handle: handle.socket_handle,
                                port: self.port,
                                is_ephemeral_port: false,
                                rx_shutdown: AtomicBool::new(false),
                            }),
                        };
                        handle.socket_handle = newhandle;
                        Ok((stream, remote_addr))
                    } else {
                        Err(Error::from(ErrorKind::WouldBlock))
                    }
                });
                match stream {
                    Ok(stream) => break Ok(stream),
                    Err(e) if e.kind() != ErrorKind::WouldBlock => {
                        tracing::warn!("TcpStream::connect failed: {}", e);
                        break Err(e);
                    }
                    _ => {
                        i += 1;
                        if i == Self::BACKLOG {
                            i = 0;
                            tracing::warn!("hit backlog");
                            break Err(ErrorKind::WouldBlock.into());
                        }
                    }
                }
            }
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
            } else if !socket.may_recv() || self.inner.rx_shutdown.load(Ordering::SeqCst) {
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
        tracing::info!("write {} bytes", buf.len());
        engine.blocking(|core| {
            let socket = core.get_mutable_socket(self.inner.socket_handle);
            if socket.can_send() {
                tracing::info!("sending");
                Ok(socket.send_slice(buf).unwrap())
            } else if !socket.may_send() {
                tracing::info!(
                    "can't send {} {} {}",
                    socket.state(),
                    socket.is_active(),
                    socket.is_open(),
                );
                Err(ErrorKind::ConnectionReset.into())
            } else {
                tracing::info!("would block");
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
            let mut core = ENGINE.core.lock().unwrap();
            if let Some(iface) = core.find_iface_for(addr) {
                match s.connect(iface.context(), addr, port) {
                    Ok(_) => return Ok(()),
                    Err(_) => return Err(ErrorKind::AddrNotAvailable.into()),
                }
            }
            drop(core);
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
        tracing::info!(
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

fn get_lo_device_and_interface(
    _notifier: std::sync::mpsc::Sender<Option<(SocketHandle, u16)>>,
) -> (Interface, Loopback) {
    let mut device = Loopback::new(Medium::Ethernet);

    // Create interface
    let mut config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into());
    config.random_seed = 0x2333;

    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::from_str("127.0.0.1").unwrap(), 8))
            .unwrap();
    });

    (iface, device)
}
