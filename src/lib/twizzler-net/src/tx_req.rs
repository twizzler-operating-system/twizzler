use crate::{addr::Ipv4Addr, req::PacketData};

#[derive(Clone, Copy, Debug)]
pub enum TxRequest {
    Echo(PacketData),
    SendToIpv4(Ipv4Addr, PacketData),
    ListenIpv4(Ipv4Addr),
    Close,
}

#[derive(Clone, Copy, Debug)]
pub enum TxCompletion {
    Nothing,
}
