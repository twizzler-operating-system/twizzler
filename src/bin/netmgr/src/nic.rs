use std::{intrinsics::copy_nonoverlapping, mem::MaybeUninit, sync::Arc};

use twizzler_net::buffer::ManagedBuffer;

use crate::{
    ethernet::{EthernetAddr, EthernetError},
    header::Header,
};

#[async_trait::async_trait]
pub trait NetworkInterface {
    async fn send_ethernet(
        &self,
        header_buffer: NicBuffer,
        buffers: &[SendableBuffer],
    ) -> Result<(), EthernetError>;
    async fn recv_ethernet(&self) -> Result<Vec<Arc<NicBuffer>>, EthernetError>;
    fn get_ethernet_addr(&self) -> EthernetAddr;
}

#[derive(Debug)]
pub struct NicBuffer {
    data: *mut u8,
    len: usize,
    data_len: usize,
}

impl NicBuffer {
    #[allow(dead_code)]
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.data, self.data_len) }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.data, self.data_len) }
    }

    pub fn allocate(len: usize) -> Self {
        Self {
            data: unsafe {
                std::alloc::alloc(std::alloc::Layout::from_size_align(len, 16).unwrap())
            },
            len,
            data_len: len,
        }
    }

    pub fn set_len(&mut self, len: usize) {
        self.data_len = len;
    }

    pub fn write_layer_headers(&mut self, start: usize, hdrs: &[&dyn Header]) -> usize {
        let mut offset = 0;
        let buffer = self.as_bytes_mut();
        for hdr in hdrs {
            let bytes = hdr.as_bytes();
            unsafe {
                copy_nonoverlapping(
                    bytes.as_ptr(),
                    buffer.as_mut_ptr().add(offset + start),
                    bytes.len(),
                );
            }
            offset += bytes.len();
        }
        offset
    }

    pub unsafe fn get_minimal_header<T: Header>(&self, off: usize) -> T {
        let slice = &self.as_bytes()[off..(off + core::mem::size_of::<T>())];
        let mut hdr = MaybeUninit::uninit();
        copy_nonoverlapping(slice.as_ptr(), hdr.as_mut_ptr() as *mut u8, slice.len());
        hdr.assume_init()
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
unsafe impl Sync for NicBuffer {}

pub enum SendableBuffer<'a> {
    #[allow(dead_code)]
    NicBuffer(NicBuffer),
    ManagedBuffer(ManagedBuffer<'a>),
}

impl<'a> SendableBuffer<'a> {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            SendableBuffer::NicBuffer(n) => n.as_bytes(),
            SendableBuffer::ManagedBuffer(m) => m.as_bytes(),
        }
    }

    #[allow(dead_code)]
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        match self {
            SendableBuffer::NicBuffer(n) => n.as_bytes_mut(),
            SendableBuffer::ManagedBuffer(m) => m.as_bytes_mut(),
        }
    }
}
