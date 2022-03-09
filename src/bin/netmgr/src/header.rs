use crate::link::nic::{NicBuffer, SendableBuffer};

pub trait Header {
    fn len(&self) -> usize;
    fn update_csum(&mut self, header_buffer: NicBuffer, buffers: &[SendableBuffer]);
    fn as_bytes(&self) -> &[u8] {
        let ptr = self as *const Self as *const u8;
        unsafe { core::slice::from_raw_parts(ptr, self.len()) }
    }
}
