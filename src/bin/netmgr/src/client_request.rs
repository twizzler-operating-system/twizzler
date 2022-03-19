use twizzler_async::Task;
use twizzler_net::{addr::Ipv4Addr, RxRequest, TxCompletion, TxRequest};
use twizzler_queue::QueueError;

use crate::{
    link::nic::{NicBuffer, SendableBuffer},
    listen,
    network::ipv4::{self, setup_ipv4_listen},
    send, HandleRef,
};

pub async fn handle_client_request(
    handle: &HandleRef,
    id: u32,
    request: TxRequest,
) -> Result<(), QueueError> {
    println!("got txreq {:?}", request);
    let reply = match request {
        TxRequest::Echo(incoming_data) => {
            let buffer = handle.get_incoming_buffer(incoming_data);
            let incoming_slice = buffer.as_bytes();
            let s = String::from_utf8(incoming_slice.to_vec());
            println!("incoming slice was {:?}", s);
            let mut buffer = handle.allocatable_buffer_controller().allocate().await;
            buffer.copy_in(b"Reply Packet Data");
            let packet_data = buffer.as_packet_data();
            let _ = handle.submit(RxRequest::EchoReply(packet_data)).await;
            println!("reply completed");
            TxCompletion::Nothing
        }
        TxRequest::SendToIpv4(addr, data) => {
            #[allow(unused_variables)]
            let buffer = handle.get_incoming_buffer(data);
            #[allow(unreachable_code)]
            let _ = ipv4::send_to(
                handle,
                Ipv4Addr::localhost(), /* TODO */
                addr,
                todo!(),
                &[SendableBuffer::ManagedBuffer(buffer)],
                NicBuffer::allocate(0x1000), /* TODO */
                None,
            )
            .await;
            //TODO: complete with error or not.
            TxCompletion::Nothing
        }
        TxRequest::ListenIpv4(addr) => {
            setup_ipv4_listen(handle.clone(), addr);
            TxCompletion::Nothing
        }
        TxRequest::Close => {
            handle.set_closed();
            TxCompletion::Nothing
        }
        TxRequest::Listen(conn_info) => listen::setup_listen(handle, conn_info),
        TxRequest::Send(conn_id, packet_data) => {
            let handle = handle.clone();
            Task::spawn(async move {
                let reply = send::send_packet(&handle, conn_id, packet_data).await;
                let _ = handle.complete(id, reply).await;
            })
            .detach();
            return Ok(());
        }
        _ => TxCompletion::Nothing,
    };
    handle.complete(id, reply).await
}
