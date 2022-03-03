use std::sync::Arc;

use twizzler_async::Task;
use twizzler_net::{NmHandle, RxCompletion, RxRequest, TxRequest};

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
            let _ = handle_clone.handle(handler).await;
        })
        .detach();

        let res = handle.submit(TxRequest::Echo(packet_data)).await.unwrap();
        println!("got txc {:?}", res);
        std::future::pending::<()>().await
    });
}

async fn handler(_handle: &Arc<NmHandle>, id: u32, req: RxRequest) -> RxCompletion {
    println!("got response {} {:?}", id, req);
    RxCompletion::Nothing
}
