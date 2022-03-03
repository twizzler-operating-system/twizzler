use crate::req::PacketData;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum RxRequest {
    EchoReply(PacketData),
}

#[derive(Clone, Copy, Debug)]
pub enum RxCompletion {
    Nothing,
}
