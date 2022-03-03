use std::sync::Arc;

use twizzler_async::Task;
use twizzler_net::{NmHandleManager, RxRequest, TxCompletion, TxRequest};

fn main() {
    println!("Hello from netmgr");

    let num_threads = 1;
    for _ in 0..num_threads {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    loop {
        let nm_handle = Arc::new(twizzler_net::server_open_nm_handle().unwrap());
        println!("manager got new nm handle!");
        Task::spawn(async move {
            loop {
                if nm_handle.handle(handler).await.is_err() {
                    break;
                }
            }
            println!("got err");
        })
        .detach();
    }
}

async fn handler(handle: &Arc<NmHandleManager>, id: u32, req: TxRequest) -> TxCompletion {
    println!("got txreq {} {:?}", id, req);
    let mut buffer = handle.allocatable_buffer_controller().allocate();
    buffer.copy_in(b"Reply Packet Data");
    let packet_data = buffer.as_packet_data();

    let _ = handle.submit(RxRequest::EchoReply(packet_data)).await;
    TxCompletion::Nothing
}
