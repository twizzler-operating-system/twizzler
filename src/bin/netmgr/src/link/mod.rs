use std::sync::Arc;

use crate::header::Header;

use self::nic::NicBuffer;

pub mod ethernet;
pub mod nic;

#[derive(Debug)]
pub struct IncomingPacketInfo {
    pub buffer: Arc<NicBuffer>,
    pub network_info: Option<(usize, usize)>, // starting byte, length in bytes
    link_info: Option<(usize, usize)>,
    transport_info: Option<(usize, usize)>,
}

impl IncomingPacketInfo {
    pub fn new(buffer: Arc<NicBuffer>) -> Self {
        Self {
            buffer,
            network_info: None,
            link_info: None,
            transport_info: None,
        }
    }

    pub fn update_for_link(mut self, hdr_off: usize, len: usize) -> Option<Self> {
        let off = hdr_off;
        if off + len > self.buffer.packet_len() {
            return None;
        }
        self.link_info = Some((off, len));

        Some(self)
    }

    pub fn update_for_network(mut self, hdr_off: usize, len: usize) -> Option<Self> {
        let prev = self.link_info.unwrap().0;
        let off = hdr_off + prev;
        if off + len > self.buffer.packet_len() {
            return None;
        }
        self.network_info = Some((off, len));

        Some(self)
    }

#[allow(dead_code)]
    pub fn update_for_transport(mut self, hdr_off: usize, len: usize) -> Option<Self> {
        let prev = self.network_info.unwrap().0;
        let off = hdr_off + prev;
        if off + len > self.buffer.packet_len() {
            return None;
        }
        self.transport_info = Some((off, len));

        Some(self)
    }

    pub fn packet_len(&self) -> usize {
        self.buffer.packet_len()
    }
    
    pub unsafe fn get_network_hdr<T: Header>(&self) -> Option<T> {
        let info = self.network_info.unwrap();
        // println!("Network header location: {:?}, header size: {:?} bytes",info.0, std::mem::size_of::<T>());
        if std::mem::size_of::<T>() > info.1 {
            println!("Bad Header. Too small to fit network neader.");
            return None;
        }
        Some(self.buffer.get_minimal_header(info.0))
    }

    #[allow(dead_code)]
    pub unsafe fn get_transport_hdr<T: Header>(&self) -> Option<T> {
        let info = self.transport_info.unwrap();
        if info.0 + std::mem::size_of::<T>() > info.1 {
            return None;
        }
        Some(self.buffer.get_minimal_header(info.0))
    }
}
