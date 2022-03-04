use crate::{
    addr::Ipv4Addr,
    req::{CloseInfo, ConnectionId, PacketData},
    tx_req::ConnectionInfo,
};

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum RxRequest {
    EchoReply(PacketData),
    RecvFromIpv4(Ipv4Addr, PacketData),
    Recv(ConnectionId, PacketData),
    CloseConnection(ConnectionId, CloseInfo),
    NewConnection(ConnectionId, ConnectionId, ConnectionInfo),
    Close,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum RxCompletion {
    Nothing,
}
