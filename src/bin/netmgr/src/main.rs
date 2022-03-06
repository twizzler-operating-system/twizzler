use std::sync::Arc;

use twizzler_async::Task;
use twizzler_net::{addr::Ipv4Addr, NmHandleManager, RxRequest, TxCompletion, TxRequest};

use crate::{
    ipv4::setup_ipv4_listen,
    nic::{NicBuffer, SendableBuffer},
};

mod arp;
mod endpoint;
mod ethernet;
mod header;
mod icmp;
mod ipv4;
mod layer4;
mod nic;
mod nics;

fn main() {
    println!("Hello from netmgr");

    nics::init();

    let num_threads = 1;
    for _ in 0..num_threads {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    loop {
        let nm_handle = Arc::new(twizzler_net::server_open_nm_handle().unwrap());
        println!("manager got new nm handle! {:?}", nm_handle);
        let _task = Task::spawn(async move {
            loop {
                if nm_handle.handle(handler).await.is_err() {
                    println!("got err");
                    break;
                }

                if nm_handle.is_terminated() {
                    if nm_handle.is_dead() {
                        println!("got err");
                    }
                    break;
                }
            }
            println!("nm_handle was closed");
        })
        .detach();
    }
}

async fn handler(handle: &Arc<NmHandleManager>, id: u32, req: TxRequest) -> TxCompletion {
    println!("got txreq {} {:?}", id, req);
    match req {
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
        }
        TxRequest::SendToIpv4(addr, data) => {
            let buffer = handle.get_incoming_buffer(data);
            let _ = ipv4::send_to(
                handle,
                Ipv4Addr::localhost(), /* TODO */
                addr,
                &[SendableBuffer::ManagedBuffer(buffer)],
                NicBuffer::allocate(0x1000), /* TODO */
                None,
            )
            .await;
            //TODO: complete with error or not.
        }
        TxRequest::ListenIpv4(addr) => {
            setup_ipv4_listen(handle.clone(), addr);
        }
        TxRequest::Close => {
            handle.set_closed();
        }
        _ => {}
    }
    TxCompletion::Nothing
}
