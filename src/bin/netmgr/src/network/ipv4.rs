use std::sync::{Arc, Mutex};

use byteorder::{ByteOrder, NetworkEndian};
use twizzler_net::addr::{Ipv4Addr, ServiceAddr};

use crate::{
    header::Header,
    link::{
        ethernet::{EtherType, EthernetAddr, EthernetHeader},
        nic::{NicBuffer, SendableBuffer},
        IncomingPacketInfo,
    },
    transport::handle_packet,
    HandleRef,
};

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct Ipv4Header {
    info1: u8,
    info2: u8,
    len: [u8; 2],
    ident: [u8; 2],
    flags_and_frag: [u8; 2],
    ttl: u8,
    prot: u8,
    csum: [u8; 2],
    source: [u8; 4],
    dest: [u8; 4],
}

impl Ipv4Header {
    #[allow(dead_code)]
    pub fn dest_addr(&self) -> Ipv4Addr {
        NetworkEndian::read_u32(&self.dest).into()
    }

    #[allow(dead_code)]
    pub fn source_addr(&self) -> Ipv4Addr {
        NetworkEndian::read_u32(&self.source).into()
    }

    pub fn packet_len(&self) -> usize {
        NetworkEndian::read_u16(&self.len) as usize
    }
}

impl Header for Ipv4Header {
    fn len(&self) -> usize {
        20 //TODO
    }

    fn update_csum(&mut self, _header_buffer: NicBuffer, _buffers: &[SendableBuffer]) {
        //no op
    }
}

fn build_ipv4_header(source: Ipv4Addr, dest: Ipv4Addr, prot: Ipv4Prot) -> Ipv4Header {
    // TODO: we should take in other args as well for the other things in the header
    let mut hdr = Ipv4Header {
        info1: 4,
        info2: 0,
        len: Default::default(),
        ident: Default::default(),
        flags_and_frag: Default::default(),
        ttl: 8, //??
        prot: prot as u8,
        csum: Default::default(),
        source: Default::default(),
        dest: Default::default(),
    };
    NetworkEndian::write_u16(&mut hdr.len, 20);
    NetworkEndian::write_u32(&mut hdr.source, source.into());
    NetworkEndian::write_u32(&mut hdr.dest, dest.into());
    // TODO: checksum
    hdr
}

pub async fn send_to(
    _handle: &HandleRef,
    source: Ipv4Addr,
    dest: Ipv4Addr,
    prot: Ipv4Prot,
    buffers: &[SendableBuffer<'_>],
    mut header_buffer: NicBuffer,
    layer4_header: Option<&(dyn Header + Sync)>,
) -> Result<(), Ipv4SendError> {
    if dest.is_localhost() {
        let lo = crate::nics::lookup_nic(&EthernetAddr::local()).ok_or(Ipv4SendError::Unknown)?;
        let header = build_ipv4_header(source, dest, prot);

        let eth_header = EthernetHeader::build_localhost(EtherType::Ipv4);
        let len = if let Some(l4) = layer4_header {
            header_buffer.write_layer_headers(0, &[&eth_header, &header, l4])
        } else {
            header_buffer.write_layer_headers(0, &[&eth_header, &header])
        };
        header_buffer.set_len(len);
        // TODO: checksums?
        return lo
            .send_ethernet(header_buffer, buffers)
            .await
            .map_err(|_| Ipv4SendError::Unknown);
    }
    todo!()
}

#[repr(u8)]
pub enum Ipv4Prot {
    Icmp = 0x01,
}

impl TryFrom<Ipv4Prot> for ServiceAddr {
    type Error = ();

    fn try_from(value: Ipv4Prot) -> Result<Self, Self::Error> {
        match value {
            Ipv4Prot::Icmp => return Ok(ServiceAddr::Icmp),
        }
    }
}

impl TryFrom<u8> for Ipv4Prot {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::Icmp),
            _ => Err(()),
        }
    }
}

pub enum Ipv4SendError {
    Unknown,
}

#[allow(dead_code)]
// TODO: This is all pretty slow probably
struct Listener {
    addr: Ipv4Addr,
    handle: HandleRef,
}

struct GlobalListener {
    listeners: Mutex<Vec<Arc<Listener>>>,
}

lazy_static::lazy_static! {
static ref LISTEN: GlobalListener = GlobalListener {
    listeners: Mutex::new(vec![]),
};
}

pub fn setup_ipv4_listen(handle: HandleRef, addr: Ipv4Addr) {
    let mut listeners = LISTEN.listeners.lock().unwrap();
    listeners.push(Arc::new(Listener { addr, handle }));
}

pub async fn handle_incoming_ipv4_packet(info: IncomingPacketInfo) {
    /*
        let header = unsafe { buffer.get_minimal_header::<Ipv4Header>(offset) };
        // TODO: checksum
        let dest_addr = header.dest_addr();
        let source_addr = header.dest_addr();
        println!("got incoming ipv4 packet for {}", dest_addr);
        {
            let listeners = LISTEN.listeners.lock().unwrap();
            for listener in listeners.iter() {
                if dest_addr == listener.addr {
                    let listener = listener.clone();
                    let buffer = buffer.clone();
                    Task::spawn(async move {
                        let mut send_buffer = listener
                            .handle
                            .allocatable_buffer_controller()
                            .allocate()
                            .await;
                        send_buffer.copy_in(&buffer.as_bytes()[(offset + header.len())..]);
                        println!("replying to client");
                        let _ = listener
                            .handle
                            .submit(RxRequest::RecvFromIpv4(
                                dest_addr,
                                send_buffer.as_packet_data(),
                            ))
                            .await;
                    })
                    .detach();
                }
            }
            drop(listeners);
        }

    */
    let header = unsafe { info.get_network_hdr::<Ipv4Header>() };
    if let Some(header) = header {
        let len = header.packet_len();
        let header_len = header.len();
        if let Some(info) = info.update_for_transport(header_len, len) {
            let prot: Result<Ipv4Prot, ()> = header.prot.try_into();
            if let Ok(prot) = prot {
                if let Ok(service_addr_any) = prot.try_into() {
                    handle_packet(service_addr_any, info).await
                }
            }
        }
    }
}
