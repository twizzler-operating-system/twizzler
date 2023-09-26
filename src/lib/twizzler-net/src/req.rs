use std::fmt::Display;


#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PacketData {
    pub(crate) buffer_idx: u32,
    pub(crate) buffer_len: u32,
}


#[repr(transparent)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct ConnectionId(u64);

impl From<u64> for ConnectionId {
    fn from(x: u64) -> Self {
        Self(x)
    }
}

#[repr(u8)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum CloseInfo {
    Rx,
    Tx,
    Both,
    Reset,
}
