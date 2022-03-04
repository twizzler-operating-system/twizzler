use crate::{
    addr::{Ipv4Addr, NodeAddr, ServiceAddr},
    req::{CloseInfo, ConnectionId, PacketData},
};

#[repr(C)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct ConnectionInfo {
    node_addr: NodeAddr,
    service_addr: ServiceAddr,
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
