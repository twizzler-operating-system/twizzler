#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PacketData {
    pub(crate) buffer_idx: u32,
    pub(crate) buffer_len: u32,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct ConnectionId(u32);

#[repr(u8)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum CloseInfo {
    Rx,
    Tx,
    Both,
    Reset,
}
