#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PacketData {
    pub(crate) buffer_idx: u32,
    pub(crate) buffer_len: u32,
}
