use crate::{addr::Ipv4Addr, req::PacketData};

#[derive(Clone, Copy, Debug)]
pub enum TxRequest {
    Echo(PacketData),
    SendToIpv4(Ipv4Addr, PacketData),
    Close,
}

#[derive(Clone, Copy, Debug)]
pub enum TxCompletion {
    Nothing,
}
