use crate::{
    addr::{Ipv4Addr, NodeAddr, ProtType, ServiceAddr},
    req::{CloseInfo, ConnectionId, PacketData},
};

bitflags::bitflags! {
    pub struct ConnectionFlags: u32 {
        const RAW = 0x1;
    }
}

#[repr(C)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct ConnectionInfo {
    node_addr: NodeAddr,
    service_addr: ServiceAddr,
    prot_type: ProtType,
    conn_flags: ConnectionFlags,
}

impl ConnectionInfo {
    pub fn new(
        node_addr: NodeAddr,
        service_addr: ServiceAddr,
        prot_type: ProtType,
        conn_flags: ConnectionFlags,
    ) -> Self {
        Self {
            node_addr,
            service_addr,
            prot_type,
            conn_flags,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum TxRequest {
    Echo(PacketData),
    SendToIpv4(Ipv4Addr, PacketData),
    ListenIpv4(Ipv4Addr),
    Connect(ConnectionInfo),
    Send(ConnectionId, PacketData),
    CloseConnection(ConnectionId, CloseInfo),
    Listen(ConnectionInfo),
    StopListen(ConnectionId),
    Close,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum TxCompletion {
    Nothing,
    ConnectionReady(ConnectionId),
    ListenReady(ConnectionId),
}
