use crate::{
    addr::{Ipv4Addr, NodeAddr, ServiceAddr},
    req::{CloseInfo, ConnectionId, PacketData},
};

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Connection {
    addr: (NodeAddr, ServiceAddr),
    peer: (NodeAddr, ServiceAddr),
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum RxRequest {
    EchoReply(PacketData),
    RecvFromIpv4(Ipv4Addr, PacketData),
    Recv(ConnectionId, PacketData),
    CloseConnection(ConnectionId, CloseInfo),
    NewConnection(ConnectionId, ConnectionId, Connection),
    Close,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum RxCompletion {
    Nothing,
}
