use std::sync::Arc;

use twizzler_async::Task;
use twizzler_net::{
    addr::{Ipv4Addr, NodeAddr, ProtType, ServiceAddr},
    ConnectionFlags, PacketData, RxRequest, TxCompletion,
};

use crate::{
    endpoint::{foreach_endpoint, EndPointKey},
    header::Header,
    ipv4::Ipv4Prot,
    nic::{NicBuffer, SendableBuffer},
    HandleRef,
};

#[repr(C)]
struct IcmpHeader {
    ty: u8,
    code: u8,
    csum: [u8; 2],
    extra: [u8; 4],
}

impl Header for IcmpHeader {
    fn len(&self) -> usize {
        8
    }

    fn update_csum(&mut self, _header_buffer: NicBuffer, _buffers: &[crate::nic::SendableBuffer]) {
        todo!()
    }
}

pub fn handle_icmp_packet(
    packet: &Arc<NicBuffer>,
    packet_start: usize,
    packet_len_inc_hdr: usize,
    source_addr: Ipv4Addr,
    dest_addr: Ipv4Addr,
) {
    let header = unsafe { packet.get_minimal_header::<IcmpHeader>(packet_start) };
    println!("got icmp packet {} {}", header.ty, header.code);

    let info = EndPointKey::new(
        NodeAddr::Ipv4(source_addr),
        NodeAddr::Ipv4(dest_addr),
        ProtType::Icmp,
        ConnectionFlags::RAW,
        ServiceAddr::Null,
        ServiceAddr::Null,
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

pub async fn send_packet(
    handle: &HandleRef,
    info: EndPointKey,
    packet_data: PacketData,
) -> TxCompletion {
    let dest_addr = info.dest_address();
    let dest_addr = match dest_addr.0 {
        NodeAddr::Ipv4(a) => a,
    };
    let source = Ipv4Addr::localhost();
    let header_buffer = NicBuffer::allocate(0x2000 /* TODO */);
    let handle = handle.clone();
    Task::spawn(async move {
        let buffer = handle.get_incoming_buffer(packet_data);
        let _ = crate::ipv4::send_to(
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
