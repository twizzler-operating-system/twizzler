use std::{
    io::ErrorKind,
    net::SocketAddr,
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
};

use secgate::util::Handle;
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    socket::tcp::{Socket, State},
    time::{Duration, Instant},
    wire::{EthernetAddress, IpAddress, IpCidr},
};
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep,
    ThreadSyncWake,
};
use twizzler_net::{NetClient, NetClientConfig};

use crate::runtime::file::socket::shim::{port::PortAssigner, TcpStreamInner, IP};

pub struct Engine {
    pub(super) core: Arc<Mutex<Core>>,
    waiter: Arc<Condvar>,
    notify: Arc<AtomicU64>,
    _polling_thread: JoinHandle<()>,
}

pub(super) struct Core {
    socketset: SocketSet<'static>,
    ifaceset: Vec<IfaceSet>,
    tracking: Vec<(SocketHandle, u16)>,
}

struct IfaceSet {
    ifaces: Vec<Interface>,
    device: NetClient,
}

impl IfaceSet {
    fn new(device: NetClient) -> Self {
        let ifaces = Vec::new();
        Self { ifaces, device }
    }

    fn insert_iface(&mut self, iface: Interface) {
        self.ifaces.push(iface);
    }

    fn poll(&mut self, socketset: &mut SocketSet<'static>) -> bool {
        let mut ready = false;
        for iface in &mut self.ifaces {
            ready |= iface.poll(Instant::now(), &mut self.device, socketset);
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

lazy_static::lazy_static! {
    pub(crate) static ref ENGINE: Arc<Engine> = Arc::new(Engine::new());
    pub(crate) static ref PORTS: Arc<PortAssigner> = Arc::new(PortAssigner::new());
}

impl Engine {
    fn new() -> Self {
        let (iface, device) = get_twznet_device_and_interface();

        let mut nic = IfaceSet::new(device);
        nic.insert_iface(iface);

        let core = Arc::new(Mutex::new(Core::new(vec![nic])));
        let waiter = Arc::new(Condvar::new());
        let notify = Arc::new(AtomicU64::new(0));
        let _inner = core.clone();
        let _waiter = waiter.clone();
        let _notify = notify.clone();

        // Okay, here is our background polling thread. It polls the network interface with the
        // SocketSet whenever it needs to, which is:
        // 1. when smoltcp says to based on poll_time() (calls poll_delay internally)
        // 2. when the state changes (eg a new socket is added)
        // 3. when blocking threads need to poll (we get a message on the channel)
        let thread = std::thread::spawn(move || {
            let inner = _inner;
            let waiter = _waiter;
            let notify = _notify;

            fn check_tracking() -> bool {
                let mut core = ENGINE.core.lock().unwrap();
                for idx in 0..core.tracking.len() {
                    let item = core.tracking[idx];
                    let socket = core.get_mutable_socket(item.0);
                    if socket.state() == State::Closed {
                        tracing::debug!("tracked tcp socket {} in closed state", item.0);
                        core.release_socket(item.0);
                        core.tracking.remove(idx);
                        drop(core);
                        PORTS.return_port(item.1);
                        return true;
                    }
                }
                false
            }

            loop {
                while check_tracking() {}
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

                let core = inner.lock().unwrap();
                let mut waiters = core
                    .ifaceset
                    .iter()
                    .map(|iface| ThreadSync::new_sleep(iface.device.rx_waiter()))
                    .collect::<Vec<_>>();
                waiters.push(ThreadSync::new_sleep(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(&*notify),
                    0,
                    twizzler_abi::syscall::ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                )));

                let any_ready = core
                    .ifaceset
                    .iter()
                    .any(|iface| iface.device.has_rx_pending());

                if !any_ready && notify.swap(0, Ordering::SeqCst) != 0 {
                    sys_thread_sync(&mut waiters, time.map(|t| t.into())).unwrap();
                }
            }
        });
        Self {
            core,
            waiter,
            notify,
            _polling_thread: thread,
        }
    }

    fn wake(&self) {
        self.notify.store(1, Ordering::SeqCst);
        sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&*self.notify),
                usize::MAX,
            ))],
            None,
        )
        .unwrap();
    }

    pub fn add_socket(&self, socket: Socket<'static>) -> SocketHandle {
        self.core.lock().unwrap().add_socket(socket)
    }

    // Block until f returns Ok(R), and then return R. Note that f may be called multiple times,
    // and it may be called spuriously. If f returns Err(e) with e.kind() anything other than
    // NonBlock, return the error.
    pub fn blocking<R>(
        &self,
        mut f: impl FnMut(&mut Core) -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        let mut core = self.core.lock().unwrap();
        if let Ok(r) = f(&mut *core) {
            return Ok(r);
        }
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
                    self.wake();
                    core = self.waiter.wait(core).unwrap();
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn track(&self, inner: &TcpStreamInner) {
        let port = if inner.is_ephemeral_port {
            inner.port
        } else {
            0
        };
        self.core
            .lock()
            .unwrap()
            .tracking
            .push((inner.socket_handle, port))
    }

    pub fn with_iface_for<R>(
        &self,
        addr: SocketAddr,
        f: impl FnOnce(&mut Interface) -> R,
    ) -> Option<R> {
        self.core.lock().unwrap().find_iface_for(addr).map(|i| f(i))
    }
}

impl Core {
    fn new(ifaceset: Vec<IfaceSet>) -> Self {
        let socketset = SocketSet::new(Vec::new());
        Self {
            socketset,
            ifaceset,
            tracking: Vec::new(),
        }
    }

    pub fn add_socket(&mut self, sock: Socket<'static>) -> SocketHandle {
        self.socketset.add(sock)
    }

    pub fn get_mutable_socket(&mut self, handle: SocketHandle) -> &mut Socket<'static> {
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

fn get_twznet_device_and_interface() -> (Interface, NetClient) {
    let mut device = NetClient::open(NetClientConfig {}).unwrap();

    // Create interface
    let mut config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into());
    config.random_seed = 0x2333;

    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::from_str(IP).unwrap(), 8))
            .unwrap();
    });

    (iface, device)
}
