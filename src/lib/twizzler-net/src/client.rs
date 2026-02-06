use std::net::IpAddr;

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
    ClientMsg, ClientMsgKind, ClientRet, INVALID_PACKET, PacketNum, PacketSet, ServerMsg,
    ServerMsgKind, ServerRet, endpoint::Pair,
};

pub struct NetClient {
    tx: Pair<ClientMsg, ServerRet>,
    rx: Pair<ServerMsg, ClientRet>,
    handle: Descriptor,
    pending_rx: PacketSet,
    pending_id: Option<u32>,
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
            self.rx.complete(pending_id, ClientRet {});
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
        cap.max_transmission_unit = 1514;
        cap.max_burst_size = Some(1);
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
        self.consumed = true;

        self.nc
            .tx
            .send_packets(&[self.packet], |s| ClientMsg {
                kind: ClientMsgKind::Tx(s),
            })
            .expect("failed to send packet");

        ret
    }
}

impl RxToken for NetClientRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mem = self.nc.rx.packet_mem_mut(self.packet);
        f(mem)
    }
}

impl Drop for NetClientTxToken<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            self.nc.tx.release_packet(self.packet);
        }
    }
}
