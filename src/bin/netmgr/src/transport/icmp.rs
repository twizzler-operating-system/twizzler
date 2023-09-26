use std::sync::Arc;

use twizzler_async::Task;
use twizzler_net::{
    addr::{Ipv4Addr, NodeAddr, ServiceAddr},
    ListenFlags, PacketData, RxRequest, TxCompletion, ListenInfo, TxRequest,
};

use crate::{
    endpoint::{foreach_endpoint, EndPointKey},
    header::Header,
    link::{
        nic::{NicBuffer, SendableBuffer, NetworkInterface,},
        IncomingPacketInfo, ethernet::{EthernetHeader, EtherType, EthernetAddr},
    },
    network::ipv4::{Ipv4Prot, Ipv4Header, Ipv4SendError, build_ipv4_header},
    HandleRef,
};

use super::TransportProto;

const ICMP_ECHO_REPLY: u8 = 0;

#[derive(Debug)]
pub struct Icmp;

#[async_trait::async_trait]
impl TransportProto for Icmp {
    async fn send_packet(
        &self,
        _handle: &HandleRef,
        _endpoint_info: EndPointKey,
        _packet_data: PacketData,
    ) -> TxCompletion {
        todo!()
    }

    async fn handle_packet(&self, _info: IncomingPacketInfo) {
        // println!("Received ping packet in ICMP packet handler. Now to reply....");
        // println!("ICMP receiver: Packet contains: {:?}",_info);
        
        // get icmp header
        let icmp_len = std::mem::size_of::<IcmpHeader>();
        let icmp_offset = _info.network_info.unwrap().0+_info.network_info.unwrap().1-icmp_len;
        let icmpheader = &_info.buffer.as_bytes()[icmp_offset..icmp_offset+icmp_len]; 
        // println!("Got buffer: {:?}", &_info.buffer.as_bytes());
         
        // println!("Got icmp header: {:?}", icmpheader); 
        let icmp_code = icmpheader[0];

        // println!("ICMP type: {:?}", icmp_code);

        // get ping sender
        let header = unsafe { _info.get_network_hdr::<Ipv4Header>() }; 
        // println!("Header: {:?}", header);
         match header {
            None => {
                assert!(true == false);
            },
            Some(hdr) => {
                let dest = hdr.dest_addr();
                if !dest.is_localhost(){
                    println!("Sending to non localhost not supported yet.");
                    todo!();
                }
            }
        }
        // if ECHO_REPLY handle it
        if icmp_code == 0 {
            println!("Reply from {}", header.unwrap().source_addr());
            // send packet up to network handler that sent request

            return;
        }

        // if ECHO_REQUEST reply to the request with an ECHO_REPLY
        let icmp_header = IcmpHeader {
            ty: ICMP_ECHO_REPLY,
            code: 0,
            csum: [0; 2],
            id: [0; 2],
            seq: [0; 2],
        };

        // println!("Preparing packet to return local ping");
        let lo1 = crate::nics::lookup_nic(&EthernetAddr::local());
        let lo = match lo1 {
            Some(l) => l,
            None => panic!("handle_packet: cannot find loopback device."),
        };
        let source = Ipv4Addr::localhost();
        let dest = Ipv4Addr::localhost();
        let prot = Ipv4Prot::Icmp; 
        let header = build_ipv4_header(source, dest, prot);
        // println!("Added ipv4 header: {:?}", header.as_bytes());
        // println!("Length of ipv4 header: {} bytes", header.len());
   
        
        let eth_header = EthernetHeader::build_localhost(EtherType::Ipv4);
        // println!("Added ethernet header: {:?}", eth_header.as_bytes());
        // println!("Length of ethernet header: {} bytes", eth_header.len());


        // ignore this layer 4 check - it's set to null in other places too
        //layer4_header: Option<&(dyn Header + Sync)>,       
        let mut header_buffer = NicBuffer::allocate(0x1000);
        let len = header_buffer.write_layer_headers(0, &[&eth_header, &header, &icmp_header]);
        header_buffer.set_len(len);

        // TODO - There is no body. Packet should return payload
        let buffers:[SendableBuffer;0] = [];
        // make a packet with that as destination
        // assume it has already reached the ethernet layer.
        // write it to the ethernet buffer
        // println!("Added ipv4 and ethernet headers to get {:?}", header_buffer.as_bytes());
        // println!("Total length of icmp + network + link layer headers= {} bytes", len);
        _= lo
            .send_ethernet(header_buffer, &buffers)
            .await;
        
    }

    fn raw_support(&self) -> super::RawSupport {
        super::RawSupport::OnlyRaw
    }
}

pub fn init() -> (ServiceAddr, Icmp) {
    // println!("Initialzing ICMP packet handler.");
    let icmp_instance = Icmp;
    (ServiceAddr::Icmp, icmp_instance) 
}

#[repr(C)]
pub struct IcmpHeader {
    ty: u8,
    code: u8,
    csum: [u8; 2],
    id: [u8; 2],
    seq: [u8; 2],
}

impl Header for IcmpHeader {
    fn len(&self) -> usize {
        8 // bytes
    }

    fn update_csum(
        &mut self,
        _header_buffer: NicBuffer,
        _buffers: &[crate::link::nic::SendableBuffer],
    ) {
        todo!()
    }
}

#[derive(Debug)]
#[repr(u8)]
pub enum IcmpType {
    IcmpReply = 0x00,
    IcmpRequest= 0x01,
}


pub fn _handle_icmp_packet(
    packet: &Arc<NicBuffer>,
    packet_start: usize,
    packet_len_inc_hdr: usize,
    source_addr: Ipv4Addr,
    dest_addr: Ipv4Addr,
) {
    let header = unsafe { packet.get_minimal_header::<IcmpHeader>(packet_start) };
    // println!("got icmp packet {} {}", header.ty, header.code);

    let info = EndPointKey::new(
        NodeAddr::Ipv4(source_addr),
        NodeAddr::Ipv4(dest_addr),
        ListenFlags::RAW,
        ServiceAddr::Icmp,
        ServiceAddr::Icmp,
    );
    foreach_endpoint(&info, |handle, conn_id| {
        let handle = Arc::clone(handle);
        let packet = Arc::clone(packet);
        Task::spawn(async move {
            let mut buffer = handle.allocatable_buffer_controller().allocate().await;
            let bytes = &packet.as_bytes()[packet_start..(packet_start + packet_len_inc_hdr)];
            buffer.copy_in(bytes);
            let _ = handle
                .submit(RxRequest::Recv(conn_id, buffer.as_packet_data()))
                .await;
        })
        .detach();
    });
}

pub async fn _send_packet(
    handle: &HandleRef,
    info: EndPointKey,
    packet_data: PacketData,
) -> TxCompletion {
    let dest_addr = info.dest_address();
    let NodeAddr::Ipv4(dest_addr) = dest_addr.0;
    let source = Ipv4Addr::localhost();
    let header_buffer = NicBuffer::allocate(0x2000 /* TODO */);
    let handle = handle.clone();
    Task::spawn(async move {
        let buffer = handle.get_incoming_buffer(packet_data);
        let _ = crate::network::ipv4::send_to(
            &handle,
            source,
            dest_addr,
            Ipv4Prot::Icmp,
            &[SendableBuffer::ManagedBuffer(buffer)],
            header_buffer,
            None,
        )
        .await;
    })
    .detach();
    TxCompletion::Nothing
}
