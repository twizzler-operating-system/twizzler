use crate::req::PacketData;

#[derive(Clone, Copy, Debug)]
pub enum TxRequest {
    Echo(PacketData),
}

#[derive(Clone, Copy, Debug)]
pub enum TxCompletion {
    Nothing,
}
