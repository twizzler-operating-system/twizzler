use crate::{
    addr::{Ipv4Addr, NodeAddr, ServiceAddr},
    req::{CloseInfo, ConnectionId, PacketData},
};

bitflags::bitflags! {
    pub struct ListenFlags: u32 {
        const RAW = 0x1;
    }
}

#[repr(C)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]

/// Connection information - Node, service, flags (e.g. RAW) 
pub struct ListenInfo {
    node_addr: NodeAddr,
    service_addr: ServiceAddr,
    flags: ListenFlags,
}

impl ListenInfo {
    pub fn address(&self) -> (NodeAddr, ServiceAddr) {
        (self.node_addr, self.service_addr)
    }

    pub fn flags(&self) -> ListenFlags {
        self.flags
    }

    pub fn new(node_addr: NodeAddr, service_addr: ServiceAddr, flags: ListenFlags) -> Self {
        Self {
            node_addr,
            service_addr,
            flags,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum TxRequest {
    Echo(PacketData),
    SendIcmpv4(Ipv4Addr,PacketData),
    SendToIpv4(Ipv4Addr, PacketData),
    ListenIpv4(Ipv4Addr),
    Connect(ListenInfo),
    Send(ConnectionId, PacketData),
    CloseConnection(ConnectionId, CloseInfo),
    Listen(ListenInfo),
    StopListen(ConnectionId),
    Close,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum TxCompletionError {
    Unknown,
    InvalidArgument,
    NoSuchConnection,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum TxCompletion {
    Nothing,
    ConnectionReady(ConnectionId),
    ListenReady(ConnectionId),
    Error(TxCompletionError),
}
