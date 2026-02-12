//! Userspace TCP networking via smoltcp for WASI socket support.
//!
//! Adapted from the test-tiny-http shim. Uses smoltcp directly (bypassing kernel
//! socket abstractions) in line with Twizzler's philosophy of keeping the kernel
//! out of the I/O path.

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use lazy_static::lazy_static;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, Medium};
use smoltcp::socket::tcp::{ConnectError, Socket, State};
use smoltcp::storage::RingBuffer;
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use virtio_net::{DeviceWrapper, TwizzlerTransport};

// ── Constants ────────────────────────────────────────────────────────

const IP: &str = "10.0.2.15"; // QEMU user networking default
const GATEWAY: &str = "10.0.2.2";
const RX_BUF_SIZE: usize = 65536;
const TX_BUF_SIZE: usize = 8192;
const BACKLOG: usize = 8;
const EPHEMERAL_START: u16 = 49152;
const EPHEMERAL_END: u16 = 65535;

// ── Types ────────────────────────────────────────────────────────────

/// Network address (IPv4 + port).
#[derive(Clone, Copy, Debug)]
pub struct NetAddr {
    pub ip: IpAddress,
    pub port: u16,
}

/// Shutdown direction.
#[derive(Clone, Copy, Debug)]
pub enum NetShutdown {
    Read,
    Write,
    Both,
}

/// Networking errors, mappable to WASI P1 errno or P2 ErrorCode.
#[derive(Debug)]
pub enum NetError {
    WouldBlock,
    ConnectionRefused,
    ConnectionReset,
    NotConnected,
    AddrInUse,
    AddrNotAvailable,
    InvalidArgument,
    NotSupported,
    PortExhaustion,
    Other(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

// ── Port Assigner ────────────────────────────────────────────────────

struct PortAssignerInner {
    unused_start: u16,
    stack: Vec<u16>,
}

impl PortAssignerInner {
    fn new() -> Self {
        Self {
            unused_start: EPHEMERAL_START,
            stack: Vec::new(),
        }
    }

    fn get_ephemeral_port(&mut self) -> Option<u16> {
        self.stack.pop().or_else(|| {
            let next = self.unused_start;
            if next == EPHEMERAL_END {
                None
            } else {
                self.unused_start += 1;
                Some(next)
            }
        })
    }

    fn return_port(&mut self, port: u16) {
        if self.unused_start == port + 1 {
            self.unused_start -= 1;
        } else {
            self.stack.push(port);
        }
    }
}

struct PortAssigner {
    inner: Mutex<PortAssignerInner>,
}

impl PortAssigner {
    fn new() -> Self {
        Self {
            inner: Mutex::new(PortAssignerInner::new()),
        }
    }

    fn get_ephemeral_port(&self) -> Option<u16> {
        self.inner.lock().unwrap().get_ephemeral_port()
    }

    fn return_port(&self, port: u16) {
        self.inner.lock().unwrap().return_port(port);
    }
}

// ── Core ─────────────────────────────────────────────────────────────

struct Core {
    socketset: SocketSet<'static>,
    iface: Interface,
    device: DeviceWrapper<TwizzlerTransport>,
}

type SocketBuffer<'a> = RingBuffer<'a, u8>;

impl Core {
    fn new(iface: Interface, device: DeviceWrapper<TwizzlerTransport>) -> Self {
        Self {
            socketset: SocketSet::new(Vec::new()),
            device,
            iface,
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
        let res = self
            .iface
            .poll(Instant::now(), &mut self.device, &mut self.socketset);
        waiter.notify_all();
        res
    }

    fn poll_time(&mut self) -> Option<Duration> {
        self.iface.poll_delay(Instant::now(), &mut self.socketset)
    }
}

// ── Engine (singleton) ───────────────────────────────────────────────

pub struct Engine {
    core: Arc<Mutex<Core>>,
    waiter: Arc<Condvar>,
    channel: mpsc::Sender<Option<(SocketHandle, u16)>>,
    _polling_thread: JoinHandle<()>,
}

lazy_static! {
    static ref ENGINE: Arc<Engine> = Arc::new(Engine::new());
    static ref PORTS: Arc<PortAssigner> = Arc::new(PortAssigner::new());
}

impl Engine {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Option<(SocketHandle, u16)>>();
        let (iface, device) = get_device_and_interface(sender.clone());
        let core = Arc::new(Mutex::new(Core::new(iface, device)));
        let waiter = Arc::new(Condvar::new());
        let inner = core.clone();
        let w = waiter.clone();

        let thread = std::thread::spawn(move || {
            let inner = inner;
            let waiter = w;
            let mut tracking = Vec::new();

            fn check_tracking(tracking: &mut Vec<(SocketHandle, u16)>) {
                let mut core = ENGINE.core.lock().unwrap();
                let removed = tracking
                    .extract_if(.., |item| {
                        let socket = core.get_mutable_socket(item.0);
                        if socket.state() == State::Closed {
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
                    if matches!(time, Some(Duration::ZERO)) {
                        inner.poll(&*waiter);
                        continue;
                    }
                    time
                };

                let inner = match time {
                    Some(dur) => receiver.recv_timeout(dur.into()).ok(),
                    None => receiver.recv().ok(),
                }
                .flatten();
                if let Some(inner) = inner {
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

    fn blocking<R>(
        &self,
        mut f: impl FnMut(&mut Core) -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        let mut core = self.core.lock().unwrap();
        core.poll(&self.waiter);
        self.wake();
        loop {
            match f(&mut *core) {
                Ok(r) => {
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

    fn track(&self, inner: &NetSocketInner) {
        let port = if inner.is_ephemeral_port {
            inner.port
        } else {
            0
        };
        let _ = self.channel.send(Some((inner.socket_handle, port)));
    }
}

// ── NetSocket ────────────────────────────────────────────────────────

struct NetSocketInner {
    socket_handle: SocketHandle,
    port: u16,
    is_ephemeral_port: bool,
    rx_shutdown: AtomicBool,
}

impl Drop for NetSocketInner {
    fn drop(&mut self) {
        ENGINE.track(self);
    }
}

/// A TCP stream backed by smoltcp.
pub struct NetSocket {
    inner: Arc<NetSocketInner>,
}

impl NetSocket {
    /// Connect to a remote TCP endpoint.
    pub fn connect(remote: NetAddr) -> Result<NetSocket, NetError> {
        let engine = &ENGINE;
        let mut sock = {
            let rx_buffer = SocketBuffer::new(vec![0; RX_BUF_SIZE]);
            let tx_buffer = SocketBuffer::new(vec![0; TX_BUF_SIZE]);
            Socket::new(rx_buffer, tx_buffer)
        };
        let ports = &PORTS;
        let port = ports
            .get_ephemeral_port()
            .ok_or(NetError::PortExhaustion)?;

        let mut core = engine.core.lock().unwrap();
        if let Err(e) = sock.connect(core.iface.context(), (remote.ip, remote.port), port) {
            ports.return_port(port);
            return Err(match e {
                ConnectError::Unaddressable => NetError::AddrNotAvailable,
                ConnectError::InvalidState => NetError::InvalidArgument,
            });
        }
        let handle = core.add_socket(sock);
        drop(core);

        Ok(NetSocket {
            inner: Arc::new(NetSocketInner {
                socket_handle: handle,
                port,
                rx_shutdown: AtomicBool::new(false),
                is_ephemeral_port: true,
            }),
        })
    }

    /// Read data from the socket (blocking).
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, NetError> {
        let engine = &ENGINE;
        engine
            .blocking(|core| {
                let socket = core.get_mutable_socket(self.inner.socket_handle);
                if socket.can_recv() {
                    Ok(socket.recv_slice(buf).unwrap())
                } else if !socket.may_recv()
                    || self.inner.rx_shutdown.load(Ordering::SeqCst)
                {
                    self.inner.rx_shutdown.store(true, Ordering::SeqCst);
                    Ok(0)
                } else {
                    Err(ErrorKind::WouldBlock.into())
                }
            })
            .map_err(io_err_to_net)
    }

    /// Write data to the socket (blocking).
    pub fn write(&self, buf: &[u8]) -> Result<usize, NetError> {
        let engine = &ENGINE;
        engine
            .blocking(|core| {
                let socket = core.get_mutable_socket(self.inner.socket_handle);
                if socket.can_send() {
                    Ok(socket.send_slice(buf).unwrap())
                } else if !socket.may_send() {
                    Err(ErrorKind::ConnectionReset.into())
                } else {
                    Err(ErrorKind::WouldBlock.into())
                }
            })
            .map_err(io_err_to_net)
    }

    /// Shutdown the socket.
    pub fn shutdown(&self, how: NetShutdown) -> Result<(), NetError> {
        let engine = &ENGINE;
        let mut core = engine.core.lock().unwrap();
        let socket = core.get_mutable_socket(self.inner.socket_handle);
        if socket.state() == State::Closed {
            return Ok(());
        }
        match how {
            NetShutdown::Read => {
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
            }
            NetShutdown::Write => {
                socket.close();
            }
            NetShutdown::Both => {
                socket.close();
                self.inner.rx_shutdown.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    /// Get the remote endpoint address.
    pub fn peer_addr(&self) -> Result<NetAddr, NetError> {
        let engine = &ENGINE;
        let mut core = engine.core.lock().unwrap();
        let socket = core.get_mutable_socket(self.inner.socket_handle);
        let remote = socket.remote_endpoint().ok_or(NetError::NotConnected)?;
        Ok(NetAddr {
            ip: remote.addr,
            port: remote.port,
        })
    }

    /// Get the local endpoint address.
    pub fn local_addr(&self) -> Result<NetAddr, NetError> {
        let engine = &ENGINE;
        let mut core = engine.core.lock().unwrap();
        let socket = core.get_mutable_socket(self.inner.socket_handle);
        let local = socket.local_endpoint().ok_or(NetError::NotConnected)?;
        Ok(NetAddr {
            ip: local.addr,
            port: local.port,
        })
    }

    /// Clone the socket (shares the same underlying connection).
    pub fn clone_socket(&self) -> NetSocket {
        NetSocket {
            inner: self.inner.clone(),
        }
    }
}

// ── NetListener ──────────────────────────────────────────────────────

/// A TCP listener backed by smoltcp.
pub struct NetListener {
    listeners: Mutex<Vec<SocketHandle>>,
    local_addr: NetAddr,
    port: u16,
}

impl NetListener {
    /// Bind and listen on the given address.
    pub fn bind(local: NetAddr) -> Result<NetListener, NetError> {
        let engine = &ENGINE;
        let mut listeners = Vec::with_capacity(BACKLOG);
        let port = local.port;

        for _ in 0..BACKLOG {
            let mut sock = {
                let rx_buffer = SocketBuffer::new(vec![0; RX_BUF_SIZE]);
                let tx_buffer = SocketBuffer::new(vec![0; TX_BUF_SIZE]);
                Socket::new(rx_buffer, tx_buffer)
            };
            if sock.listen(port).is_err() {
                return Err(NetError::AddrInUse);
            }
            let handle = engine.add_socket(sock);
            listeners.push(handle);
        }

        Ok(NetListener {
            listeners: Mutex::new(listeners),
            local_addr: local,
            port,
        })
    }

    /// Accept an incoming connection (blocking).
    pub fn accept(&self) -> Result<(NetSocket, NetAddr), NetError> {
        let engine = &ENGINE;
        let mut i: usize = 0;
        engine
            .blocking(|core| {
                loop {
                    let result = {
                        let listeners = self.listeners.lock().unwrap();
                        let handle = listeners[i];
                        let sock = core.get_mutable_socket(handle);
                        if sock.is_active() {
                            let remote = sock.remote_endpoint().unwrap();
                            Some((
                                handle,
                                NetAddr {
                                    ip: remote.addr,
                                    port: remote.port,
                                },
                            ))
                        } else {
                            None
                        }
                    };

                    if let Some((handle, remote_addr)) = result {
                        // Create replacement listener socket
                        let mut new_sock = {
                            let rx_buffer = SocketBuffer::new(vec![0; RX_BUF_SIZE]);
                            let tx_buffer = SocketBuffer::new(vec![0; TX_BUF_SIZE]);
                            Socket::new(rx_buffer, tx_buffer)
                        };
                        if new_sock.listen(self.port).is_err() {
                            return Err(std::io::Error::other("listen error on replacement"));
                        }
                        let new_handle = core.add_socket(new_sock);

                        // Swap handle in the listeners list
                        let mut listeners = self.listeners.lock().unwrap();
                        listeners[i] = new_handle;

                        let stream = NetSocket {
                            inner: Arc::new(NetSocketInner {
                                socket_handle: handle,
                                port: self.port,
                                is_ephemeral_port: false,
                                rx_shutdown: AtomicBool::new(false),
                            }),
                        };
                        return Ok((stream, remote_addr));
                    }

                    i += 1;
                    if i == BACKLOG {
                        i = 0;
                        return Err(ErrorKind::WouldBlock.into());
                    }
                }
            })
            .map_err(io_err_to_net)
    }

    /// Get the local address.
    pub fn local_addr(&self) -> Result<NetAddr, NetError> {
        Ok(self.local_addr)
    }
}

// ── Device Initialization ────────────────────────────────────────────

fn get_device_and_interface(
    notifier: mpsc::Sender<Option<(SocketHandle, u16)>>,
) -> (Interface, DeviceWrapper<TwizzlerTransport>) {
    use std::str::FromStr;
    use virtio_net::get_device;

    let mut device = get_device(notifier);

    if device.capabilities().medium != Medium::Ethernet {
        panic!("Only virtio-net ethernet devices are supported");
    }

    let hardware_addr = HardwareAddress::Ethernet(device.mac_address());
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

// ── Helpers ──────────────────────────────────────────────────────────

fn io_err_to_net(e: std::io::Error) -> NetError {
    match e.kind() {
        ErrorKind::WouldBlock => NetError::WouldBlock,
        ErrorKind::ConnectionRefused => NetError::ConnectionRefused,
        ErrorKind::ConnectionReset => NetError::ConnectionReset,
        ErrorKind::NotConnected => NetError::NotConnected,
        ErrorKind::AddrInUse => NetError::AddrInUse,
        ErrorKind::AddrNotAvailable => NetError::AddrNotAvailable,
        ErrorKind::InvalidInput => NetError::InvalidArgument,
        _ => NetError::Other(e.to_string()),
    }
}
