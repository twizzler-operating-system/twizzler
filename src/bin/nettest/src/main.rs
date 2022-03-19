use std::{sync::Arc, time::Duration};

use twizzler_async::{Task, Timer};
use twizzler_net::{
    addr::{Ipv4Addr, NodeAddr, ServiceAddr},
    buffer::ManagedBuffer,
    ListenFlags, ListenInfo, NmHandle, RxCompletion, RxRequest, TxRequest,
};

#[repr(C)]
struct IcmpHeader {
    ty: u8,
    code: u8,
    csum: [u8; 2],
    extra: [u8; 4],
}

const ICMP_ECHO_REQUEST: u8 = 8;

fn handle_ping_recv(_buffer: ManagedBuffer) {
    println!("nettest ping recv");
}

fn fill_ping_buffer(_idx: usize, buffer: &mut ManagedBuffer) {
    let icmp_header = IcmpHeader {
        ty: ICMP_ECHO_REQUEST,
        code: 0,
        csum: [0; 2],
        extra: [0; 4],
    };
    buffer.get_data_mut(0).write(icmp_header);
}

fn ping(addr: Ipv4Addr) {
    let handle = Arc::new(twizzler_net::open_nm_handle("ping test").unwrap());

    // Run the async ping code.
    twizzler_async::run(async {
        // Build a new connection info. It's not really a "connection", more of a way to specify a
        // place to listen at. For ping, that's ipv4+icmp, raw.
        let conn_info = ListenInfo::new(NodeAddr::Ipv4(addr), ServiceAddr::Icmp, ListenFlags::RAW);

        println!("sending listen");
        // Start listening here.
        let tx_cmp = handle.submit(TxRequest::Listen(conn_info)).await.unwrap();

        // In response, we get back a connection ID that we can use.
        let listen_id = match tx_cmp {
            twizzler_net::TxCompletion::ListenReady(conn_id) => conn_id,
            _ => panic!("some err"),
        };

        println!("got new listen id {:?}", listen_id);

        // Clone the handle for use in the recv task.
        let handle_clone = handle.clone();
        // Create a receiver task. This task will receive ping responses and then print out ping
        // status messages.
        Task::spawn(async move {
            // Loop until the handle call fails.
            while handle_clone
                .handle(|handle, _id, req| async move {
                    // We got an RxRequest! See what it is.
                    match req {
                        RxRequest::Recv(conn_id, packet_data) => {
                            if conn_id == listen_id {
                                // It's a receive on our connection. Grab the incoming buffer and
                                // handle the ping response.
                                let buffer = handle.get_incoming_buffer(packet_data);
                                handle_ping_recv(buffer);
                            }
                        }
                        // If we need to close, then do so.
                        RxRequest::Close => handle.set_closed(),
                        _ => {}
                    };
                    // Respond to the net manager
                    RxCompletion::Nothing
                })
                .await
                .is_ok()
            {}
        })
        .detach();

        // Meanwhile, submit some pings!
        for i in 0..4 {
            // Let's grab a buffer...
            let mut buffer = handle.allocatable_buffer_controller().allocate().await;
            // And fill out that buffer with a ping packet...
            fill_ping_buffer(i, &mut buffer);
            println!("sending ping buffer");
            // ...and then submit it!
            let _ = handle
                .submit(TxRequest::Send(listen_id, buffer.as_packet_data()))
                .await;
            Timer::after(Duration::from_millis(1000)).await;
            // TODO: or send-to?
        }
    });
}

fn main() {
    println!("Hello from nettest!");
    let handle = Arc::new(twizzler_net::open_nm_handle("nettest").unwrap());
    println!("nettest got nm handle: {:?}", handle);

    ping(Ipv4Addr::localhost());

    twizzler_async::run(async move {
        let mut buffer = handle.allocatable_buffer_controller().allocate().await;
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
            let mut buffer = handle.allocatable_buffer_controller().allocate().await;
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
