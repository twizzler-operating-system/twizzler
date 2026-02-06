use secgate::TwzError;
use smoltcp::phy::{DeviceCapabilities, Medium, RxToken, TxToken};
use twizzler::object::{MapFlags, Object, RawObject};
use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_io::packet::PacketObject;
use twizzler_queue::{Queue, QueueBase};

use crate::{
    ClientMsg, ClientMsgKind, ClientRet, INVALID_PACKET, PacketNum, PacketSet, ServerMsg,
    ServerMsgKind, ServerRet, client::NetClientOpenInfo, endpoint::Pair,
};

pub struct NetServer {
    client_tx: Pair<ClientMsg, ServerRet>,
    client_rx: Pair<ServerMsg, ClientRet>,
    pending_client_tx: PacketSet,
    pending_client_id: Option<u32>,
}

impl NetServer {
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
        cap.max_transmission_unit = 1514;
        cap.max_burst_size = Some(1);
        cap
    }
}

pub struct NetServerTxToken<'a> {
    ns: &'a NetServer,
    packet: PacketNum,
    consumed: bool,
}

pub struct NetServerRxToken<'a> {
    ns: &'a NetServer,
    packet: PacketNum,
}

impl TxToken for NetServerTxToken<'_> {
    fn consume<R, F>(mut self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        if len > self.ns.client_rx.packet_size() {
            panic!("packet size exceeded");
        }
        let mem = self.ns.client_rx.packet_mem_mut(self.packet);
        let ret = f(&mut mem[0..len]);
        self.consumed = true;

        self.ns
            .client_rx
            .send_packets(&[self.packet], |s| ServerMsg {
                kind: ServerMsgKind::Tx(s),
            })
            .expect("failed to send packet");

        ret
    }
}

impl RxToken for NetServerRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mem = self.ns.client_tx.packet_mem_mut(self.packet);
        f(mem)
    }
}

impl Drop for NetServerTxToken<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            self.ns.client_rx.release_packet(self.packet);
        }
    }
}
