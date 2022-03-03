use twizzler_net::buffer::ManagedBuffer;

use crate::ethernet::{EthernetAddr, EthernetError};

#[async_trait::async_trait]
pub trait NetworkInterface {
    async fn send_ethernet(&self, buffers: &[ManagedBuffer]) -> Result<(), EthernetError>;
    async fn recv_ethernet(&self) -> Result<Vec<NicBuffer>, EthernetError>;
    fn get_ethernet_addr(&self) -> EthernetAddr;
}

pub struct NicBuffer {
    data: *mut u8,
    len: usize,
}

impl NicBuffer {
    #[allow(dead_code)]
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.data, self.len) }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.data, self.len) }
    }

    pub fn allocate(len: usize) -> Self {
        Self {
            data: unsafe {
                std::alloc::alloc(std::alloc::Layout::from_size_align(len, 16).unwrap())
            },
            len,
        }
    }
}

impl Drop for NicBuffer {
    fn drop(&mut self) {
        unsafe {
            std::alloc::dealloc(
                self.data,
                std::alloc::Layout::from_size_align(self.len, 16).unwrap(),
            );
        }
    }
}

unsafe impl Send for NicBuffer {}
