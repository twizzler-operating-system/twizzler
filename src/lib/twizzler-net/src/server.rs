use secgate::TwzError;
use smoltcp::phy::{DeviceCapabilities, Medium, RxToken, TxToken};
use twizzler::object::{MapFlags, Object, RawObject};
use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_io::packet::PacketObject;
use twizzler_queue::{Queue, QueueBase};

use crate::{
    ClientMsg, ClientMsgKind, ClientRet, INVALID_PACKET, MAX_PACKETS_SET, PacketNum, PacketSet,
    ServerMsg, ServerMsgKind, ServerRet, client::NetClientOpenInfo, endpoint::Pair,
};

use crate::client::LOCAL_MTU;

pub struct NetServer {
    client_tx: Pair<ClientMsg, ServerRet>,
    client_rx: Pair<ServerMsg, ClientRet>,
    pending_client_tx: PacketSet,
    pending_client_id: Option<u32>,
}

impl NetServer {
    pub fn client_tx_packet_object(&self) -> &PacketObject {
        self.client_tx.packet_object()
    }

    pub fn rx_waiter(&self) -> ThreadSyncSleep {
        self.client_tx.rx_waiter()
    }

    pub fn completions_waiter(&self) -> ThreadSyncSleep {
        self.client_rx.comp_waiters()
    }

    pub fn has_pending_msg_from_client(&self) -> bool {
        self.client_tx.has_pending_msg()
            || self
                .pending_client_tx
                .0
                .iter()
                .any(|p| *p != INVALID_PACKET)
    }

    pub fn open(info: &NetClientOpenInfo) -> Result<Self, TwzError> {
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
            client_tx: tx,
            client_rx: rx,
            pending_client_id: None,
            pending_client_tx: PacketSet::new(),
        })
    }
}

impl NetServer {
    /// Deliver up to `MAX_PACKETS_SET` frames to this client in **one** queue message.
    ///
    /// Returns how many were accepted. A short return means the client's rx pool ran dry and the
    /// remaining frames were not delivered; the caller must count those as drops, exactly as the
    /// one-frame-at-a-time path did. A backed-up client is dropped rather than blocking the
    /// switch, and retrying here would deadlock: the pool is drained by that client's own thread,
    /// which needs the handles lock this caller is holding.
    ///
    /// The transport has always carried `MAX_PACKETS_SET` packets per message -- only the send
    /// half submitted one at a time, wasting seven slots and a doorbell per frame. The receiving
    /// client already drains a multi-packet message through successive `receive()` calls
    /// (`client.rs`, `pending_rx`), so nothing on that side changes.
    pub fn inject(&mut self, frames: &[&[u8]]) -> usize {
        self.client_rx.check_completions();
        let mut packets = [INVALID_PACKET; MAX_PACKETS_SET];
        let mut n = 0;
        for frame in frames.iter().take(MAX_PACKETS_SET) {
            // Cannot happen while LOCAL_MTU <= the slot size, but `TxToken::consume` *panics* on
            // it and a panic here takes the whole net service down. Stop rather than skip: a hole
            // in the middle of a batch reorders everything after it.
            if frame.len() > self.client_rx.packet_size() {
                break;
            }
            let Some(p) = self.client_rx.allocate_packet() else {
                break;
            };
            self.client_rx.packet_mem_mut(p)[..frame.len()].copy_from_slice(frame);
            self.client_rx.set_packet_len(p, frame.len());
            packets[n] = p;
            n += 1;
        }
        if n > 0 {
            self.submit_rx(&packets[..n]);
        }
        n
    }

    /// The one place frames are handed to the client. Both the batch path (`inject`) and smoltcp's
    /// one-token-at-a-time `TxToken::consume` route through here, so there is a single submission
    /// site to reason about rather than two that can drift.
    fn submit_rx(&self, packets: &[PacketNum]) {
        self.client_rx
            .send_packets(packets, |s| ServerMsg {
                kind: ServerMsgKind::Tx(s),
            })
            .expect("failed to send packets");
    }
}

impl smoltcp::phy::Device for NetServer {
    type RxToken<'a>
        = NetServerRxToken<'a>
    where
        Self: 'a;

    type TxToken<'a>
        = NetServerTxToken<'a>
    where
        Self: 'a;

    fn receive(
        &mut self,
        timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let idx = self
            .pending_client_tx
            .0
            .iter()
            .position(|x| *x != INVALID_PACKET);
        if let Some(idx) = idx {
            let next = self.pending_client_tx.0[idx];
            self.pending_client_tx.0[idx] = INVALID_PACKET;
            self.client_rx.check_completions();

            return Some((
                NetServerRxToken {
                    ns: self,
                    packet: next,
                },
                NetServerTxToken {
                    ns: self,
                    packet: self.client_rx.allocate_packet().unwrap(),
                    consumed: false,
                },
            ));
        }

        if let Some(pending_id) = self.pending_client_id.take() {
            self.client_tx.complete(pending_id, ServerRet {});
        }

        let (id, msg) = self.client_tx.recv_msg()?;
        self.pending_client_id = Some(id);
        match msg.kind {
            ClientMsgKind::Tx(packet_set) => {
                self.pending_client_tx = packet_set;
            }
        }
        self.receive(timestamp)
    }

    fn transmit(&mut self, _timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        self.client_rx.check_completions();
        let packet = self.client_rx.allocate_packet()?;
        Some(NetServerTxToken {
            ns: self,
            packet,
            consumed: false,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut cap = DeviceCapabilities::default();
        cap.medium = Medium::Ethernet;
        cap.max_transmission_unit = LOCAL_MTU;
        // smoltcp clamps the advertised TCP receive window to `max_burst_size * MSS`. It is a
        // guard for devices whose network buffers are far smaller than their TCP buffers (its own
        // example driver has four); at `Some(1)` it pins the window to a single segment, which is
        // stop-and-wait TCP. A full 64 KiB window is ~45 segments and this pool has 1024 slots, so
        // the pool depth is the honest bound -- it leaves the socket buffer as the limit, and
        // tightens automatically if anyone shrinks the pool.
        cap.max_burst_size = Some(self.client_rx.nr_packets());
        cap
    }
}

pub struct NetServerTxToken<'a> {
    ns: &'a NetServer,
    pub packet: PacketNum,
    consumed: bool,
}

pub struct NetServerRxToken<'a> {
    ns: &'a NetServer,
    pub packet: PacketNum,
}

impl TxToken for NetServerTxToken<'_> {
    fn consume<R, F>(mut self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        if len > self.ns.client_rx.packet_size() {
            panic!(
                "packet size exceeded ({} {})",
                len,
                self.ns.client_rx.packet_size()
            );
        }
        let mem = self.ns.client_rx.packet_mem_mut(self.packet);
        let ret = f(&mut mem[0..len]);
        self.ns.client_rx.set_packet_len(self.packet, len);
        self.consumed = true;

        self.ns.submit_rx(&[self.packet]);

        ret
    }
}

impl RxToken for NetServerRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        let len = self.ns.client_tx.packet_len(self.packet);
        let mem = self.ns.client_tx.packet_mem_mut(self.packet);
        f(&mem[0..len])
    }
}

impl Drop for NetServerTxToken<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            self.ns.client_rx.release_packet(self.packet);
        }
    }
}
