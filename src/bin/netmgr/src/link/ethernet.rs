use std::sync::Arc;

use byteorder::ByteOrder;
use byteorder::NetworkEndian;

use crate::header::Header;
use crate::link::nic::NicBuffer;
use crate::link::IncomingPacketInfo;
use crate::network::ipv4::handle_incoming_ipv4_packet;

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct EthernetAddr {
    bytes: [u8; 6],
}

impl From<[u8; 6]> for EthernetAddr {
    fn from(x: [u8; 6]) -> Self {
        Self { bytes: x }
    }
}

impl EthernetAddr {
    #[allow(dead_code)]
    pub fn broadcast() -> Self {
        Self { bytes: [0xff; 6] }
    }

    pub fn local() -> Self {
        Self { bytes: [0; 6] }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum EthernetError {
    #[allow(dead_code)]
    Unknown,
}

#[derive(Default, Clone, Copy, Debug)]
#[repr(C)]
pub struct EthernetHeader {
    dest_mac: EthernetAddr,
    src_mac: EthernetAddr,
    ethertype: [u8; 2],
}

#[derive(Clone, Copy, Debug)]
#[repr(u16)]
pub enum EtherType {
    Ipv4 = 0x0800,
}

impl From<EtherType> for u16 {
    fn from(x: EtherType) -> Self {
        x as u16
    }
}

impl TryFrom<u16> for EtherType {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0x0800 => Ok(EtherType::Ipv4),
            _ => Err(()),
        }
    }
}

impl EthernetHeader {
    pub fn build_localhost(ethertype: EtherType) -> Self {
        let mut hdr = Self::default();
        NetworkEndian::write_u16(&mut hdr.ethertype, ethertype.into());
        hdr
    }

    pub fn ethertype(&self) -> Result<EtherType, ()> {
        NetworkEndian::read_u16(&self.ethertype).try_into()
    }
}

impl Header for EthernetHeader {
    fn len(&self) -> usize {
        14
    }

    fn update_csum(
        &mut self,
        _header_buffer: crate::link::nic::NicBuffer,
        _buffers: &[crate::link::nic::SendableBuffer],
    ) {
        //no-op
    }
}

pub async fn handle_incoming_ethernet_packets(buffers: &[Arc<NicBuffer>]) {
    println!("got incoming eth packets");
    for buffer in buffers {
        let eth = unsafe { buffer.get_minimal_header::<EthernetHeader>(0) };
        // TODO: look at dest addr
        println!("ethernet packet type {:?}", eth.ethertype());
        let incoming_info = IncomingPacketInfo::new(buffer.clone());
        if let Some(incoming_info) = incoming_info.update_for_link(0, buffer.packet_len()) {
            if let Some(incoming_info) =
                incoming_info.update_for_network(14, buffer.packet_len() - 14)
            {
                if let Ok(et) = eth.ethertype() {
                    match et {
                        EtherType::Ipv4 => handle_incoming_ipv4_packet(incoming_info).await,
                    }
                }
            }
        }
    }
}
