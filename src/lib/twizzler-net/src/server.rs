use smoltcp::phy::{ChecksumCapabilities, DeviceCapabilities, Medium, RxToken, TxToken};

use crate::{
    ClientMsg, ClientMsgKind, ClientRet, INVALID_PACKET, PacketNum, PacketSet, ServerMsg,
    ServerMsgKind, ServerRet, endpoint::Pair,
};

pub struct NetServer {
    client_tx: Pair<ClientMsg, ServerRet>,
    client_rx: Pair<ServerMsg, ClientRet>,
    pending_client_tx: PacketSet,
    pending_client_id: Option<u32>,
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
            return Some((
                NetServerRxToken {
                    ns: self,
                    packet: next,
                },
                NetServerTxToken {
                    ns: self,
                    packet: self.client_rx.allocate_packet().unwrap(),
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
        let packet = self.client_rx.allocate_packet()?;
        Some(NetServerTxToken { ns: self, packet })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut cap = DeviceCapabilities::default();
        cap.medium = Medium::Ip;
        cap.max_transmission_unit = 1500;
        cap.max_burst_size = None;
        cap.checksum = ChecksumCapabilities::ignored();
        cap
    }
}

pub struct NetServerTxToken<'a> {
    ns: &'a NetServer,
    packet: PacketNum,
}

pub struct NetServerRxToken<'a> {
    ns: &'a NetServer,
    packet: PacketNum,
}

impl TxToken for NetServerTxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        if len > self.ns.client_rx.packet_size() {
            panic!("packet size exceeded");
        }
        let mem = self.ns.client_rx.packet_mem_mut(self.packet);
        let ret = f(&mut mem[0..len]);

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
