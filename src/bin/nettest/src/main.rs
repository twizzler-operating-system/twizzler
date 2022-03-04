use std::{sync::Arc, time::Duration};

use twizzler_async::Task;
use twizzler_net::{addr::Ipv4Addr, NmHandle, RxCompletion, RxRequest, TxRequest};

fn main() {
    println!("Hello from nettest!");
    let handle = Arc::new(twizzler_net::open_nm_handle().unwrap());
    println!("nettest got nm handle");

    twizzler_async::run(async move {
        let mut buffer = handle.allocatable_buffer_controller().allocate();
        buffer.copy_in(b"Some Packet Data");
        let packet_data = buffer.as_packet_data();

        let handle_clone = handle.clone();
        Task::spawn(async move {
            loop {
                let _ = handle_clone.handle(handler).await;
            }
        })
        .detach();

        let res = handle.submit(TxRequest::Echo(packet_data)).await.unwrap();
        println!("got txc {:?}", res);

        let res = handle
            .submit(TxRequest::ListenIpv4(Ipv4Addr::localhost()))
            .await;
        println!("setup listen: {:?}", res);

        loop {
            twizzler_async::Timer::after(Duration::from_millis(1000)).await;

            println!("sending...");
            let mut buffer = handle.allocatable_buffer_controller().allocate();
            buffer.copy_in(b"Some Ipv4 Packet Data");
            let packet_data = buffer.as_packet_data();
            let res = handle
                .submit(TxRequest::SendToIpv4(Ipv4Addr::localhost(), packet_data))
                .await;
            println!("send got: {:?}", res);
        }
    });
}

async fn handler(handle: &Arc<NmHandle>, id: u32, req: RxRequest) -> RxCompletion {
    println!("got response {} {:?}", id, req);
    match req {
        RxRequest::EchoReply(incoming_data) => {
            let buffer = handle.get_incoming_buffer(incoming_data);
            let incoming_slice = buffer.as_bytes();
            let s = String::from_utf8(incoming_slice.to_vec());
            println!("response incoming slice was {:?}", s);
        }
        RxRequest::RecvFromIpv4(addr, incoming_data) => {
            let buffer = handle.get_incoming_buffer(incoming_data);
            let incoming_slice = buffer.as_bytes();
            let s = String::from_utf8(incoming_slice.to_vec());
            println!("====== >> recv incoming slice was {:?} from {}", s, addr);
        }
        RxRequest::Close => {
            handle.set_closed();
        }
        _ => {}
    }
    RxCompletion::Nothing
}
