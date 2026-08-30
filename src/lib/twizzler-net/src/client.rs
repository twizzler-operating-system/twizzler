use std::{cell::RefCell, net::IpAddr};

use monitor_api::CompartmentHandle;
use secgate::{
    TwzError,
    util::{Descriptor, Handle},
};
use smoltcp::{
    phy::{DeviceCapabilities, Medium, RxToken, TxToken},
    wire::EthernetAddress,
};
use twizzler::object::{MapFlags, ObjID, Object, RawObject};
use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_io::packet::PacketObject;
use twizzler_queue::{Queue, QueueBase};

use crate::{
    ClientMsg, ClientMsgKind, ClientRet, INVALID_PACKET, MAX_PACKETS_SET, PacketNum, PacketSet,
    ServerMsg, ServerMsgKind, ServerRet, endpoint::Pair,
};

/// Ethernet MTU advertised to smoltcp by both ends of the local delivery path.
///
/// A named const, not a literal at the call site, so which arm a build actually was can be read
/// out of the source with one grep rather than inferred from a file mtime -- a constant flipped
/// before a build window is invisible to any `find -newermt` audit. The MSS sweep
/// (prereg-mss-0827.md) moves this and nothing else: 1514 / 4014 / 9014, same bytes per
/// iteration, to separate per-frame cost from per-byte cost.
///
/// Bounded above by the packet slot size in net-srv's `twz_net_open_client` (16 KiB): a frame
/// larger than a slot panics in `NetServerTxToken::consume`. net-srv's own `MTU` const stays at
/// 1514 in every arm on purpose -- it is the threshold of the oversized-frame counter for the NIC
/// path, i.e. the detector that says a jumbo frame escaped local delivery, not a knob.
pub const LOCAL_MTU: usize = 1514;

pub struct NetClient {
    tx: Pair<ClientMsg, ServerRet>,
    rx: Pair<ServerMsg, ClientRet>,
    handle: Descriptor,
    pending_rx: PacketSet,
    pending_id: Option<u32>,
    /// Frames written by smoltcp this poll but not yet submitted, and how many.
    ///
    /// smoltcp's `Device` hands out one `TxToken` per packet and has no batch entry point, so
    /// submitting inside `consume` costs a queue message -- and possibly a wake -- per packet, on
    /// a transport whose `PacketSet` carries eight. The receive direction already batches this way
    /// (`pending_rx` drains one message into up to eight `receive()` calls); this is the same
    /// trick pointed the other way.
    ///
    /// Interior mutability because `NetClientTxToken` holds `&NetClient`: `receive()` hands out an
    /// rx and a tx token from one borrow, so the tx token cannot hold `&mut`.
    pending_tx: RefCell<([PacketNum; MAX_PACKETS_SET], usize)>,
    /// We owe the server a completion that the ring had no space for.
    ///
    /// One flag for the whole subqueue, which the MPSC submission queue does *not* guarantee is
    /// enough: it permits several producers, and two of them would race here -- one deferring while
    /// the other clears the flag, stranding the deferral with no registered waiter and no fallback
    /// timeout left to rescue it. It is sound only because every producer of this subqueue runs
    /// inside `Core::poll`, and every `Core::poll` call site holds `ENGINE.core`. **Anything that
    /// submits or completes on this pair from outside that lock breaks this flag**, not just its
    /// performance.
    comp_deferred: bool,
    pub info: NetClientOpenInfo,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct NetClientOpenInfo {
    pub tx_buf: ObjID,
    pub rx_buf: ObjID,
    pub tx_queue: ObjID,
    pub rx_queue: ObjID,
    pub handle: Descriptor,
    pub addr: IpAddr,
    pub addr_prefix_len: u8,
    pub gateway: IpAddr,
    pub hwaddr: EthernetAddress,
}

impl NetClient {
    pub fn rx_waiter(&self) -> ThreadSyncSleep {
        self.rx.rx_waiter()
    }

    /// The wake reason `rx_waiter` does not cover: the server completing a tx message is what
    /// returns packet slots, and `transmit()` returning `None` for want of one is how egress
    /// stalls. Reclaim was previously visible only to a poll that had already happened for some
    /// other reason -- which is what `Pair::progress` reports and why the poll loop needed a
    /// timeout to be correct rather than merely prompt.
    pub fn tx_completions_waiter(&self) -> ThreadSyncSleep {
        self.tx.comp_waiters()
    }

    /// Space in the rx completion ring, and only while we owe one. See `Pair::comp_space_waiter`
    /// for why this must not be registered unconditionally.
    pub fn rx_completion_space_waiter(&self) -> Option<ThreadSyncSleep> {
        self.comp_deferred.then(|| self.rx.comp_space_waiter())
    }

    /// Raw state of the rx submission ring, for the case `has_rx_pending()` says false.
    ///
    /// Returns `(bell, tail, nonempty, turn)`. `has_rx_pending` is the AND of the last two, and
    /// which one is false is the whole question when a poll thread sleeps with frames outstanding:
    /// `nonempty == false` means nothing was ever submitted, `turn == false` means entries are
    /// present and invisible to `receive` as well, which no wake can repair.
    pub fn rx_pending_parts(&self) -> (u64, u64, bool, bool) {
        self.rx.pending_parts()
    }

    pub fn has_rx_pending(&self) -> bool {
        self.rx.has_pending_msg() || self.pending_rx.0.iter().any(|p| *p != INVALID_PACKET)
    }
}

pub fn net_open_client(config: NetClientConfig) -> Result<NetClientOpenInfo, TwzError> {
    let comp = CompartmentHandle::lookup("net")?;
    let gate = unsafe { comp.dynamic_gate("twz_net_open_client") }?;
    (gate)(config)
}

pub fn net_drop_client(desc: u32) -> Result<(), TwzError> {
    let comp = CompartmentHandle::lookup("net")?;
    let gate = unsafe { comp.dynamic_gate("twz_net_drop_client") }?;
    (gate)(desc)
}

pub fn net_alloc_port(desc: Descriptor, port: Option<u16>) -> Result<u16, TwzError> {
    let comp = CompartmentHandle::lookup("net")?;
    let gate = unsafe { comp.dynamic_gate("twz_net_alloc_port") }?;
    (gate)(desc, port)
}

pub fn net_release_port(desc: Descriptor, port: u16) -> Result<(), TwzError> {
    // The thread id is the link the wedge hunt was missing: a transcript's unmatched `()` names the
    // stuck call, and the kernel's wait table names every thread's sleep word -- but nothing tied
    // the two together, so the one thread that matters could not be found in the table.
    twizzler_abi::klog_println!(
        "() net_release_port: desc = {}, port = {}, thread = {}",
        desc,
        port,
        twizzler_abi::syscall::sys_thread_self_id()
    );
    let comp = CompartmentHandle::lookup("net")?;
    twizzler_abi::klog_println!("(2) net_release_port: desc = {}, port = {}", desc, port);
    let gate = unsafe { comp.dynamic_gate("twz_net_release_port") }?;
    twizzler_abi::klog_println!("(3) net_release_port: desc = {}, port = {}", desc, port);
    (gate)(desc, port)
}

impl secgate::util::Handle for NetClient {
    type OpenError = TwzError;

    type OpenInfo = NetClientConfig;

    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let info = net_open_client(info)?;
        let tx_queue = Object::<QueueBase<ClientMsg, ServerRet>>::map(
            info.tx_queue,
            MapFlags::READ | MapFlags::WRITE,
        )?;
        let rx_queue = Object::<QueueBase<ServerMsg, ClientRet>>::map(
            info.rx_queue,
            MapFlags::READ | MapFlags::WRITE,
        )?;
        let tx = Pair::new(
            PacketObject::from(Object::map(info.tx_buf, MapFlags::READ | MapFlags::WRITE)?),
            Queue::from(tx_queue.handle().clone()),
        );
        let rx = Pair::new(
            PacketObject::from(Object::map(info.rx_buf, MapFlags::READ | MapFlags::WRITE)?),
            Queue::from(rx_queue.handle().clone()),
        );
        Ok(Self {
            tx,
            rx,
            handle: info.handle,
            pending_id: None,
            pending_rx: PacketSet::new(),
            pending_tx: RefCell::new(([INVALID_PACKET; MAX_PACKETS_SET], 0)),
            comp_deferred: false,
            info,
        })
    }

    fn release(&mut self) {
        let _ = net_drop_client(self.handle);
    }
}

impl Drop for NetClient {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct NetClientConfig {}

impl NetClient {
    /// Whether this poll reclaimed tx packets -- progress smoltcp's `PollResult` cannot see.
    pub fn took_progress(&self) -> bool {
        self.tx.take_progress()
    }

    /// Hold `packet` for submission with the rest of this poll's egress.
    ///
    /// Submits early only when the batch is full, so a burst never stalls behind a partial one.
    fn queue_tx(&self, packet: PacketNum) {
        let batch = {
            let mut pending = self.pending_tx.borrow_mut();
            let n = pending.1;
            pending.0[n] = packet;
            pending.1 = n + 1;
            if pending.1 < MAX_PACKETS_SET {
                return;
            }
            pending.1 = 0;
            pending.0
        };
        self.submit_tx(&batch);
    }

    /// Submit whatever this poll queued. **Must be called after every `Interface::poll`**, which
    /// is the only thing that drives `transmit()`; miss it and a partial batch waits for the next
    /// poll rather than going out. That is the same shape as the `shutdown()` defect where egress
    /// was queued with nothing to push it.
    pub fn flush_tx(&self) {
        let batch = {
            let mut pending = self.pending_tx.borrow_mut();
            let n = pending.1;
            if n == 0 {
                return;
            }
            pending.1 = 0;
            (pending.0, n)
        };
        self.submit_tx(&batch.0[..batch.1]);
    }

    fn submit_tx(&self, packets: &[PacketNum]) {
        let msg = |s| ClientMsg {
            kind: ClientMsgKind::Tx(s),
        };
        if !crate::NONBLOCK_POLL_QUEUE {
            self.tx.send_packets(packets, msg).expect("send packets");
            return;
        }
        // Runs inside `Core::poll` with the engine core mutex held. A full ring is the server
        // being behind, which TCP already handles by retransmitting; blocking here instead
        // stalls every socket in this compartment until something outside releases it.
        // `try_send_packets` returns the slots to the pool on failure, so a drop is a drop and
        // not also a leak.
        if self.tx.try_send_packets(packets, msg).is_err() {
            crate::note_pollq(&crate::POLLQ_TX_DROPPED, "client tx dropped");
        }
    }
}

impl smoltcp::phy::Device for NetClient {
    type RxToken<'a>
        = NetClientRxToken<'a>
    where
        Self: 'a;

    type TxToken<'a>
        = NetClientTxToken<'a>
    where
        Self: 'a;

    fn receive(
        &mut self,
        timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let idx = self.pending_rx.0.iter().position(|x| *x != INVALID_PACKET);
        if let Some(idx) = idx {
            let next = self.pending_rx.0[idx];
            self.pending_rx.0[idx] = INVALID_PACKET;
            self.tx.check_completions();

            return Some((
                NetClientRxToken {
                    nc: self,
                    packet: next,
                },
                NetClientTxToken {
                    nc: self,
                    packet: self.tx.allocate_packet().unwrap(),
                    consumed: false,
                },
            ));
        }

        if let Some(pending_id) = self.pending_id.take() {
            if self.rx.try_complete(pending_id, ClientRet {}) {
                self.comp_deferred = false;
            } else {
                self.comp_deferred = true;
                // Keep the id and stop receiving. Completing is what returns the server's packet
                // slots, so we must not drop it, and we must not take a new message on top of it
                // -- but blocking for ring space here would hold the core mutex indefinitely.
                self.pending_id = Some(pending_id);
                crate::note_pollq(&crate::POLLQ_COMP_DEFERRED, "client completion deferred");
                return None;
            }
        }

        let (id, msg) = self.rx.recv_msg()?;
        self.pending_id = Some(id);
        match msg.kind {
            ServerMsgKind::Tx(packet_set) => {
                self.pending_rx = packet_set;
            }
        }
        self.receive(timestamp)
    }

    fn transmit(&mut self, _timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        self.tx.check_completions();
        let packet = self.tx.allocate_packet()?;
        Some(NetClientTxToken {
            nc: self,
            packet,
            consumed: false,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut cap = DeviceCapabilities::default();
        cap.medium = Medium::Ethernet;
        cap.max_transmission_unit = LOCAL_MTU;
        // See NetServer::capabilities: Some(1) pins the TCP window to one segment.
        cap.max_burst_size = Some(self.rx.nr_packets());
        cap
    }
}

pub struct NetClientTxToken<'a> {
    nc: &'a NetClient,
    packet: PacketNum,
    consumed: bool,
}

pub struct NetClientRxToken<'a> {
    nc: &'a NetClient,
    packet: PacketNum,
}

impl TxToken for NetClientTxToken<'_> {
    fn consume<R, F>(mut self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        if len > self.nc.tx.packet_size() {
            panic!("packet size exceeded");
        }
        let mem = self.nc.tx.packet_mem_mut(self.packet);
        let ret = f(&mut mem[0..len]);
        self.nc.tx.set_packet_len(self.packet, len);
        self.consumed = true;
        self.nc.queue_tx(self.packet);
        ret
    }
}

impl RxToken for NetClientRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        let len = self.nc.rx.packet_len(self.packet);
        let mem = self.nc.rx.packet_mem_mut(self.packet);
        f(&mem[0..len])
    }
}

impl Drop for NetClientTxToken<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            self.nc.tx.release_packet(self.packet);
        }
    }
}
