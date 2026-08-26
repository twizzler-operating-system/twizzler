use std::{
    collections::HashMap,
    io::ErrorKind,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
    usize,
};

use secgate::{
    util::{Descriptor, Handle},
    TwzError,
};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    socket::{
        dns::Socket as DnsSocket,
        tcp::{Socket, State},
        udp::Socket as SmolUdpSocket,
    },
    time::{Duration, Instant},
    wire::{IpAddress, IpCidr, Ipv4Address, Ipv6Address},
};
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep,
    ThreadSyncWake,
};
use twizzler_net::{net_alloc_port, net_release_port, NetClient, NetClientConfig};
use twizzler_rt_abi::bindings::{wait_kind, WAIT_READ, WAIT_WRITE};

pub struct Engine {
    pub(super) core: Arc<Mutex<Core>>,
    waiter: Arc<Condvar>,
    notify: Arc<AtomicU64>,
    _polling_thread: JoinHandle<()>,
    nc_handle: Descriptor,
}

#[derive(Clone, Copy)]
pub(super) enum SockKind {
    Tcp,
    Udp,
}

/// Identity of a readiness source. Usually one socket -- but a listener's accept queue is several
/// listening sockets whose readiness is the OR over the group, and `accept()` swaps handles in and
/// out of that group, so no individual `SocketHandle` can name the thing a poller registered
/// against.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum WaitKey {
    Sock(SocketHandle),
    Group(u64),
}

/// A listening socket is readable exactly when `accept()` will return a connection for it: an
/// established one. Not `is_active()`, which smoltcp makes true from SynReceived onward -- see the
/// handshake note at the bottom.
///
/// Deliberately a state predicate rather than an event, so the readiness cannot be lost between the
/// poll pass that observes it and the `accept()` that consumes it. `SmolTcpListener::can_read` must
/// agree with this: a live check that disagrees with the published word is what makes an
/// edge-triggered consumer wait for an edge that already happened.
///
/// It must *not* also cover "this socket has broken and needs rebinding" (`!is_open()`,
/// `!is_listening()`), which is what the old retire-on-first-connection branch in `poll` tested
/// for. Those are maintenance, not readiness: advertising them makes a poller report readable while
/// `accept()` finds nothing active, takes its repair branch, and blocks -- an outright hang, which
/// is how this was found. The repair still happens, because a blocked `accept()` re-runs on every
/// `Core::poll` condvar notify rather than waiting on these words.
///
/// Excluding SYN-RECEIVED is the handshake note. `is_active()` is already true there, which offers
/// up a connection whose handshake has not finished; closing such a socket is unrecoverable, and
/// still is as of smoltcp 0.13.1 (re-checked on upgrade, line numbers from its `socket/tcp.rs`):
/// `close()` moves SYN-RECEIVED straight to FIN-WAIT-1 (1077), and `(sent_syn, sent_fin)` is
/// derived from the *current state* rather than from history (1566), so FIN-WAIT-1 reports "sent a
/// FIN, no outstanding SYN". The still-unacknowledged SYN is now invisible, `tx_buffer_start_seq`
/// is one too low (1780), and when the handshake's ACK finally arrives it satisfies
/// `tx_buffer.len() + 1 == ack_len` and is read as an ACK *of a FIN that was never transmitted*
/// (1787). The socket settles in FIN-WAIT-2 (1941), no FIN ever reaches the wire, and the peer
/// waits for an EOF that cannot come.
///
/// Waiting for the handshake closes that window and is what accept() is supposed to mean anyway.
/// The connection stays pending in the backlog meanwhile, and the transition to ESTABLISHED happens
/// inside a poll pass, which republishes readiness -- so a waiter still gets its edge.
///
/// The cost, which is real and currently unmitigated: a handshake that never completes (the client
/// vanished, or its final ACK is lost for good) pins a backlog slot invisibly and forever. It is
/// not ready, so it is never accepted; `accept()`'s repair branch only fires on `!is_open()`, and
/// SYN-RECEIVED is open, so it is never rebound; and smoltcp only reaps such a socket via
/// `timed_out()`, which needs `Socket::set_timeout`, defaults to `None`, and is never set here.
/// With `BACKLOG` of them a listener goes silently deaf. The fix would be a timeout on the
/// *listening* sockets, cleared in `accept()` -- note it must be cleared, because `remote_last_ts`
/// is refreshed on every received packet, so a timeout left on an accepted socket would abort
/// idle-but-healthy connections.
pub(super) fn listener_socket_ready(socket: &Socket<'_>) -> bool {
    socket.is_active() && socket.state() != State::SynReceived
}

/// Read-readiness of a connected stream: bytes buffered, or a close that makes the next read
/// return `Ok(0)` rather than block. Dropping the second term -- as bare `can_recv()` does --
/// leaves a poller asleep on a half-closed connection, waiting for data that by definition will
/// never arrive.
///
/// Call this from `read`'s branch condition, from `can_read`, and from `socket_readiness`, exactly
/// as `listener_socket_ready` is called from the listener's three. A readiness check that disagrees
/// with the read it predicts is the bug; one function is what stops the two drifting apart again.
/// In `read`'s EOF arm `can_recv()` is already false, so the call reduces to the close term.
///
/// `rx_shutdown` is per-fd state rather than socket state, so it cannot be read from a `&Socket`
/// and has to be passed. `socket_readiness` has no fd to read it from and passes `false`; see the
/// note there.
///
/// The pre-connection states are excluded because `may_recv()` is false there too, and a socket
/// still completing its handshake is not at EOF. No current path exposes an fd in `SynSent` --
/// nonblocking `connect` returns `InProgress` before constructing one -- so this is presently
/// belt-and-braces, and worth keeping for the nonblocking-connect implementation that would.
pub(super) fn stream_socket_ready(socket: &Socket<'_>, rx_shutdown: bool) -> bool {
    socket.can_recv()
        || ((!socket.may_recv() || rx_shutdown)
            && socket.state() != State::SynReceived
            && socket.state() != State::SynSent)
}

/// Read-readiness of a datagram socket, on the same contract as `stream_socket_ready`.
pub(super) fn udp_socket_ready(socket: &SmolUdpSocket<'_>, rx_shutdown: bool) -> bool {
    socket.can_recv() || !socket.is_open() || rx_shutdown
}

pub(super) struct Core {
    socketset: SocketSet<'static>,
    ifaceset: Vec<IfaceSet>,
    tracking: Vec<(SocketHandle, u16, SockKind)>,
    // Listening sockets -> the listener group they belong to. Membership is also what marks a
    // socket as a listener, which is what selects listener_socket_ready over can_recv/can_send.
    groups: HashMap<SocketHandle, u64>,
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
            // 0.13 returns PollResult rather than a bool; SocketStateChanged is the old `true`.
            ready |= iface.poll(Instant::now(), &mut self.device, socketset)
                == smoltcp::iface::PollResult::SocketStateChanged;
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
    fn find_iface_for_dns(&mut self) -> Option<&mut Interface> {
        // TODO
        self.ifaces.get_mut(0)
    }
}

lazy_static::lazy_static! {
    pub(crate) static ref ENGINE: Arc<Engine> = Arc::new(Engine::new());
    pub(crate) static ref WAITERS: Arc<Waiters> = Arc::new(Waiters::default());
}

struct Wait {
    read: Arc<AtomicU64>,
    write: Arc<AtomicU64>,
    // Monotonic count of falling edges (ready -> not ready) on each side, never reset. An
    // edge-triggered consumer samples this when it suppresses a readiness it has already
    // reported, and re-arms once it moves. That is the piece the level words cannot express:
    // they only say what is true *now*, so a drain followed by a refill while nobody was
    // looking is indistinguishable from the readiness never having gone away -- which would
    // leave an edge-triggered waiter suppressed forever with data pending.
    read_down: Arc<AtomicU64>,
    write_down: Arc<AtomicU64>,
}

impl Wait {
    // Both level words start "not ready" (0). Claiming readiness before the engine thread has
    // ever observed this socket would make poll/select/kevent report a bogus immediate
    // ready; the live smoltcp check (SocketKind::is_ready, OR'd in by every consumer)
    // covers the window until the first Core::poll pass.
    pub fn new() -> Self {
        Self {
            read: Arc::new(AtomicU64::new(0)),
            write: Arc::new(AtomicU64::new(0)),
            read_down: Arc::new(AtomicU64::new(0)),
            write_down: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[derive(Default)]
pub(crate) struct Waiters {
    map: Mutex<HashMap<WaitKey, Wait>>,
    groups: Mutex<GroupIds>,
}

#[derive(Default)]
struct GroupIds {
    next: u64,
    free: Vec<u64>,
}

impl Waiters {
    /// Mint an identity for a new listener group. Ids are recycled, so the map stays bounded by the
    /// peak number of concurrently live groups and a `Wait`, once created, is never freed -- the
    /// same contract init_waiter documents, for the same reason.
    pub fn alloc_group(&self) -> u64 {
        let id = {
            let mut ids = self.groups.lock().unwrap();
            match ids.free.pop() {
                Some(id) => id,
                None => {
                    let id = ids.next;
                    ids.next += 1;
                    id
                }
            }
        };
        self.init_waiter(WaitKey::Group(id));
        id
    }

    pub fn free_group(&self, id: u64) {
        self.groups.lock().unwrap().free.push(id);
    }

    // Returns an owning clone of the wait word rather than just a raw pointer into it. The
    // underlying allocation is in practice permanently stable once created (see
    // init_waiter's comment), but callers that can retain the clone (kqueue/poll/select)
    // should still do so -- it costs nothing and keeps this API's contract independent of
    // that implementation detail.
    pub fn waitpoint(
        &self,
        key: WaitKey,
        kind: wait_kind,
    ) -> Result<(Arc<AtomicU64>, u64), TwzError> {
        let mut map = self.map.lock().unwrap();
        let entry = map.entry(key).or_insert_with(|| Wait::new());
        let arc = match kind {
            x if x == WAIT_READ => entry.read.clone(),
            x if x == WAIT_WRITE => entry.write.clone(),
            _ => return Err(TwzError::INVALID_ARGUMENT),
        };
        Ok((arc, 0))
    }

    /// The falling-edge counter for `kind`, plus its current value. Sleeping on it with that
    /// value blocks until the next ready -> not-ready transition; retaining the value and
    /// comparing later tells you whether one has happened since.
    pub fn down_waitpoint(
        &self,
        key: WaitKey,
        kind: wait_kind,
    ) -> Result<(Arc<AtomicU64>, u64), TwzError> {
        let mut map = self.map.lock().unwrap();
        let entry = map.entry(key).or_insert_with(|| Wait::new());
        let arc = match kind {
            x if x == WAIT_READ => entry.read_down.clone(),
            x if x == WAIT_WRITE => entry.write_down.clone(),
            _ => return Err(TwzError::INVALID_ARGUMENT),
        };
        let val = arc.load(Ordering::SeqCst);
        Ok((arc, val))
    }

    fn mark_waiter(&self, key: WaitKey, read: bool, write: bool) {
        if let Some(wait) = self.map.lock().unwrap().get(&key) {
            let mut rfell = false;
            let mut wfell = false;
            let rwake = if read {
                wait.read.swap(1, Ordering::SeqCst) == 0
            } else {
                rfell = wait.read.swap(0, Ordering::SeqCst) == 1;
                if rfell {
                    wait.read_down.fetch_add(1, Ordering::SeqCst);
                }
                false
            };
            let wwake = if write {
                wait.write.swap(1, Ordering::SeqCst) == 0
            } else {
                wfell = wait.write.swap(0, Ordering::SeqCst) == 1;
                if wfell {
                    wait.write_down.fetch_add(1, Ordering::SeqCst);
                }
                false
            };

            // Only wake a side that actually transitioned not-ready -> ready. Waking
            // unconditionally turns the rare spurious wakeup that sys_thread_sync callers must
            // tolerate into a per-poll-cycle event, which breaks the single-shot
            // poll/select/kevent waits (they would return early with nothing ready).
            //
            // A falling edge is a wakeup too: an edge-triggered consumer suppressed on this side
            // is blocked on the down counter waiting for exactly this.
            //
            // Stack-allocated: this runs once per waiter per poll cycle, and the common case
            // pushes nothing at all, so a Vec here was a heap allocation per socket per poll for
            // a list that can never exceed four entries.
            let mut wakes = [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&*wait.read),
                usize::MAX,
            )); 4];
            let mut n = 0;
            for (fired, word) in [
                (rwake, &wait.read),
                (wwake, &wait.write),
                (rfell, &wait.read_down),
                (wfell, &wait.write_down),
            ] {
                if fired {
                    wakes[n] = ThreadSync::new_wake(ThreadSyncWake::new(
                        ThreadSyncReference::Virtual(&**word),
                        usize::MAX,
                    ));
                    n += 1;
                }
            }
            if n > 0 {
                let _ = sys_thread_sync(&mut wakes[..n], None);
            }
        }
    }

    // Resets (rather than replaces) any existing entry for `key`, so the underlying AtomicU64
    // allocation for a given key value, once created, is never freed for the life of the
    // process -- it is only ever reset back to Wait::new()'s initial state. This keeps a raw
    // `*const AtomicU64` handed out for this key (e.g. across the extern "C"
    // twz_rt_fd_waitpoint ABI, which cannot carry an owning Arc back to its caller -- see
    // ReferenceRuntime::fd_waitpoint) permanently valid even after the socket or group behind
    // it is released and its identity reused. The map can only grow to the peak number of
    // concurrently live keys: smoltcp's SocketSet reuses freed handles rather than handing out
    // ever-increasing values, and alloc_group recycles group ids for the same reason.
    //
    // The falling-edge counters are deliberately not reset: they are monotonic for the life of
    // the process, so a suppression token held over an identity reuse can only ever read as
    // "it moved" (a spurious re-arm, harmless) and never as "it did not" (a silent hang).
    fn init_waiter(&self, key: WaitKey) {
        let mut map = self.map.lock().unwrap();
        let wait = map.entry(key).or_insert_with(Wait::new);
        wait.read.store(0, Ordering::SeqCst);
        wait.write.store(0, Ordering::SeqCst);
    }
}

impl Engine {
    fn new() -> Self {
        let (iface, device) = get_twznet_device_and_interface();

        let nc_handle = device.info.handle;
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
                    let is_closed = match item.2 {
                        SockKind::Tcp => core.get_mutable_socket(item.0).state() == State::Closed,
                        // TODO: this causes some kind of stall.
                        SockKind::Udp => false, /* not core.get_mutable_udp_socket(item.0).
                                                 * is_open(), */
                    };
                    if is_closed {
                        core.release_socket(item.0);
                        core.tracking.remove(idx);
                        drop(core);
                        // `track` stores 0 for a socket whose port it does not own (see
                        // Engine::track); returning it would decrement a refcount net-srv never
                        // incremented.
                        if item.1 != 0 {
                            ENGINE.return_port(item.1);
                        }
                        return true;
                    }
                }
                false
            }

            // Refilled, not rebuilt: the set changes only when an interface comes or goes, but the
            // loop below runs on every poll cycle.
            let mut waiters: Vec<ThreadSync> = Vec::new();
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
                waiters.clear();
                waiters.extend(
                    core.ifaceset
                        .iter()
                        .map(|iface| ThreadSync::new_sleep(iface.device.rx_waiter())),
                );
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
                drop(core);
                let n = notify.swap(0, Ordering::SeqCst);
                if !any_ready && n == 0 {
                    let _ = sys_thread_sync(&mut waiters, time.map(|t| t.into()));
                }
            }
        });
        // Temporary (wedgehunt.md): every wedged transcript has five threads of one compartment
        // queued on one word at a fixed object offset, and which word decides the story -- the core
        // mutex means a dead lock holder, the condvar means the poll thread stopped notifying. The
        // kernel's wait table prints the object offset, so printing these addresses names it.
        twizzler_abi::klog_println!(
            "ENGINEADDR core {:p} waiter {:p} notify {:p}",
            &*core,
            &*waiter,
            &*notify,
        );
        Self {
            core,
            waiter,
            notify,
            _polling_thread: thread,
            nc_handle,
        }
    }

    pub fn allocate_port(&self, port: Option<u16>) -> Option<u16> {
        let r = net_alloc_port(self.nc_handle, port);
        r.ok()
    }

    pub fn return_port(&self, port: u16) {
        let _ = net_release_port(self.nc_handle, port);
    }

    pub fn get_ephemeral_port(&self) -> Option<u16> {
        self.allocate_port(None)
    }

    pub(super) fn wake(&self) {
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

    pub fn add_socket(&self, socket: Socket<'static>, group: Option<u64>) -> SocketHandle {
        self.core.lock().unwrap().add_socket(socket, group)
    }

    pub fn add_udp_socket(&self, socket: SmolUdpSocket<'static>) -> SocketHandle {
        self.core.lock().unwrap().add_udp_socket(socket)
    }

    // Block until f returns Ok(R), and then return R. Note that f may be called multiple times,
    // and it may be called spuriously. If f returns Err(e) with e.kind() anything other than
    // NonBlock, return the error.
    pub fn blocking<R>(
        &self,
        non_block: bool,
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
                    if non_block {
                        return Err(e);
                    }
                    self.wake();
                    core = self.waiter.wait(core).unwrap();
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn track(&self, handle: SocketHandle, port: u16, is_ephem: bool, kind: SockKind) {
        let port = if is_ephem { port } else { 0 };
        self.core
            .lock()
            .unwrap()
            .tracking
            .push((handle, port, kind))
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
            groups: HashMap::new(),
        }
    }

    pub fn add_dns_socket(&mut self, sock: DnsSocket<'static>) -> SocketHandle {
        self.socketset.add(sock)
    }

    pub fn add_udp_socket(&mut self, sock: SmolUdpSocket<'static>) -> SocketHandle {
        let handle = self.socketset.add(sock);
        WAITERS.init_waiter(WaitKey::Sock(handle));
        handle
    }

    /// Add a socket, optionally as a member of listener group `group` (see WaitKey).
    pub fn add_socket(&mut self, sock: Socket<'static>, group: Option<u64>) -> SocketHandle {
        let handle = self.socketset.add(sock);
        match group {
            // The group's words belong to the group, not to any one member: resetting them here
            // would drop a sibling's pending connection on the floor.
            Some(group) => {
                self.groups.insert(handle, group);
            }
            None => WAITERS.init_waiter(WaitKey::Sock(handle)),
        }
        handle
    }

    fn wait_key(&self, handle: SocketHandle) -> WaitKey {
        match self.groups.get(&handle) {
            Some(&group) => WaitKey::Group(group),
            None => WaitKey::Sock(handle),
        }
    }

    /// Take `handle` out of its listener group and give it a readiness identity of its own -- what
    /// `accept()` does to the socket it hands back as a stream.
    pub fn detach_from_group(&mut self, handle: SocketHandle) {
        if self.groups.remove(&handle).is_some() {
            WAITERS.init_waiter(WaitKey::Sock(handle));
        }
    }

    /// Queue `handle` for release once it closes, taking it out of any listener group first: a dead
    /// socket left in a group would pin the group readable forever (listener_socket_ready is true
    /// for a closed socket), which spins every poller watching it.
    pub fn retire_socket(&mut self, handle: SocketHandle) {
        if let Some(group) = self.groups.remove(&handle) {
            self.refresh_group(group);
        }
        self.tracking.push((handle, 0, SockKind::Tcp));
    }

    pub fn get_mutable_socket(&mut self, handle: SocketHandle) -> &mut Socket<'static> {
        self.socketset.get_mut(handle)
    }

    pub fn get_mutable_udp_socket(&mut self, handle: SocketHandle) -> &mut SmolUdpSocket<'static> {
        self.socketset.get_mut(handle)
    }

    pub fn get_mutable_dns_socket(&mut self, handle: SocketHandle) -> &mut DnsSocket<'static> {
        self.socketset.get_mut(handle)
    }

    pub fn release_socket(&mut self, handle: SocketHandle) {
        let group = self.groups.remove(&handle);
        self.socketset.remove(handle);
        match group {
            Some(group) => {
                // The group outlives its members, so recompute rather than forcing it ready --
                // unless this was the last one, in which case the identity itself is done.
                self.refresh_group(group);
                if !self.groups.values().any(|g| *g == group) {
                    WAITERS.free_group(group);
                }
            }
            // Closing must wake both a blocked reader and a blocked writer, so mark both sides
            // ready: mark_waiter only wakes a side it transitions to ready.
            None => WAITERS.mark_waiter(WaitKey::Sock(handle), true, true),
        }
    }

    /// Readiness of one socket, as its wait key's contributor. A listening socket's is "accept()
    /// has work here" rather than "there are bytes to read"; see listener_socket_ready.
    fn socket_readiness(
        &self,
        handle: SocketHandle,
        sock: &smoltcp::socket::Socket<'static>,
    ) -> (bool, bool) {
        match sock {
            // rx_shutdown is per-fd and invisible here, so it is passed as false: this publishes
            // the socket's own readiness. A locally read-shutdown fd is still reported ready by
            // can_read, which does see the flag, and poll ORs the two.
            smoltcp::socket::Socket::Udp(socket) => {
                (udp_socket_ready(socket, false), socket.can_send())
            }
            smoltcp::socket::Socket::Tcp(socket) => {
                if self.groups.contains_key(&handle) {
                    // Write is meaningless for a listener; SmolTcpListener::can_write agrees.
                    (listener_socket_ready(socket), false)
                } else {
                    (stream_socket_ready(socket, false), socket.can_send())
                }
            }
            _ => (false, false),
        }
    }

    /// Re-publish one readiness source's state into its wait words, using the same expressions
    /// poll() uses. Call this after an app-side operation changes readiness -- a read that drains
    /// the receive buffer, a write that fills the send buffer, an accept that takes a connection
    /// out of a listener group. Without it the words only move on the background poll pass, so a
    /// drain followed by a refill before the next pass is never observed as a not-ready period at
    /// all, which is fatal for any edge-triggered consumer (see the EV_CLEAR handling in
    /// runtime::file::kqueue).
    pub fn refresh_waiter(&self, handle: SocketHandle) {
        self.refresh_key(self.wait_key(handle));
    }

    pub fn refresh_group(&self, group: u64) {
        self.refresh_key(WaitKey::Group(group));
    }

    fn refresh_key(&self, key: WaitKey) {
        let (mut read, mut write) = (false, false);
        for (handle, sock) in self.socketset.iter() {
            if self.wait_key(handle) != key {
                continue;
            }
            let (r, w) = self.socket_readiness(handle, sock);
            read |= r;
            write |= w;
            // A Sock key has exactly one contributor; only a group needs the whole scan.
            if matches!(key, WaitKey::Sock(_)) {
                break;
            }
        }
        WAITERS.mark_waiter(key, read, write);
    }

    fn poll(&mut self, waiter: &Condvar) -> bool {
        let mut res = false;
        for ifaceset in &mut self.ifaceset {
            res |= ifaceset.poll(&mut self.socketset);
        }
        if res {
            // Aggregate before publishing: several listening sockets share one WaitKey, and marking
            // them one at a time would let a not-ready sibling clear a ready one's word (and count
            // a bogus falling edge while doing it).
            let mut agg: HashMap<WaitKey, (bool, bool)> = HashMap::new();
            for (handle, sock) in self.socketset.iter() {
                let (read, write) = self.socket_readiness(handle, sock);
                let entry = agg.entry(self.wait_key(handle)).or_insert((false, false));
                entry.0 |= read;
                entry.1 |= write;
            }
            for (key, (read, write)) in agg {
                WAITERS.mark_waiter(key, read, write);
            }
            // Notify the CV so that other waiting threads can retry their blocking operations.
            waiter.notify_all();
        }
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

    pub fn iface_for_dns(&mut self) -> Option<&mut Interface> {
        for ifaceset in &mut self.ifaceset {
            if let Some(iface) = ifaceset.find_iface_for_dns() {
                return Some(iface);
            }
        }
        None
    }
}

fn get_twznet_device_and_interface() -> (Interface, NetClient) {
    let mut device = NetClient::open(NetClientConfig {}).unwrap();

    // Create interface
    let mut config = Config::new(device.info.hwaddr.into());
    config.random_seed = std::random::random(..);

    // Static-address override. net-srv hands out addresses in the order compartments happen to
    // open a client, which is fine for talking to the outside world but leaves two compartments
    // unable to name each other ahead of time -- so a test that needs a client and a server in
    // separate compartments can pin both. The MAC still comes from net-srv, and net-srv's
    // on-host delivery is keyed on MAC, so overriding the address here needs no cooperation from
    // it (and slirp NATs whatever source address it sees).
    let addr = match std::env::var("TWZ_NET_ADDR")
        .ok()
        .and_then(|a| a.parse::<std::net::IpAddr>().ok())
    {
        Some(addr) => {
            tracing::info!("using static address {} from TWZ_NET_ADDR", addr);
            addr
        }
        None => device.info.addr,
    };

    tracing::info!(
        "setting up interface with addr {} and prefix {}",
        addr,
        device.info.addr_prefix_len
    );
    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(
                IpAddress::from(addr),
                device.info.addr_prefix_len,
            ))
            .unwrap();
    });
    match device.info.gateway {
        std::net::IpAddr::V4(ipv4_addr) => iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::from(ipv4_addr))
            .unwrap(),
        std::net::IpAddr::V6(ipv6_addr) => iface
            .routes_mut()
            .add_default_ipv6_route(Ipv6Address::from(ipv6_addr))
            .unwrap(),
    };

    (iface, device)
}
