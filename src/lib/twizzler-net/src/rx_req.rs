use crate::{req::PacketData, addr::Ipv4Addr};

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum RxRequest {
    EchoReply(PacketData),
    RecvFromIpv4(Ipv4Addr, PacketData),
    Close,
}

#[derive(Clone, Copy, Debug)]
pub enum RxCompletion {
    Nothing,
}
