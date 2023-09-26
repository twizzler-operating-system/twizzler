use std::{sync::Arc, time::Duration};

use twizzler_async::{Task, Timer};
use twizzler_net::{
    addr::{Ipv4Addr, NodeAddr, ServiceAddr},
    buffer::ManagedBuffer,
    ListenFlags, ListenInfo, NmHandle, RxCompletion, RxRequest, TxRequest,
};


#[repr(C)]
pub struct IcmpHeader {
    ty: u8,
    code: u8,
    csum: [u8; 2],
    id: [u8; 2],
    seq: [u8; 2],
}

const ICMP_ECHO_REQUEST: u8 = 8;


fn handle_ping_recv(_buffer: ManagedBuffer) {
    println!("nettest ping recv");
}

fn fill_ping_buffer(_id: u16, _seq: u16, buffer: &mut ManagedBuffer) {
    let mut icmp_header = IcmpHeader {
        ty: ICMP_ECHO_REQUEST,
        code: 0,
        csum: [0; 2],
        id: [0; 2],
        seq: [0; 2],
    };
    // fill identifier
    icmp_header.id = _id.to_le_bytes();

    // fill sequence number
    icmp_header.seq = _seq.to_le_bytes(); 

    buffer.get_data_mut(0).write(icmp_header);
    // println!("filled ping buffer with icmp header {:?}", buffer.as_bytes())
}

fn ping(addr: Ipv4Addr) {
    // get a communication handle
    let handle = Arc::new(twizzler_net::open_nm_handle("ping").unwrap());

    // Spawn a new thread
    // Run the async ping code.
    twizzler_async::run(async {
        // Build a new connection info. It's not really a "connection", more of a way to specify a
        // place to listen at. For ping, that's ipv4+icmp, raw.
        let conn_info = ListenInfo::new(NodeAddr::Ipv4(addr), ServiceAddr::Icmp, ListenFlags::RAW);

        // println!("sending listen");
        // Send an async listen transaction to the handle with the connection info above
        // to the sending queue in the network handle
        // it will return a completion when that has been submitted
        let tx_cmp = handle.submit(TxRequest::Listen(conn_info)).await.unwrap();

        // In response, we get back a connection ID that we can use for transmitting and receiving.
        let listen_id = match tx_cmp {
            twizzler_net::TxCompletion::ListenReady(conn_id) => conn_id,
            _ => panic!("some err"),
        };

        // println!("got new listen id {:?}", listen_id);

        // Clone the handle for use in the recv task.
        let handle_clone = handle.clone();
        // Create a receiver task. This task will receive ping responses and then print out ping
        // status messages.
        Task::spawn(async move {
            // Loop until the handle call fails.
            // println!("Waiting for ping receive requests.");
            while handle_clone
                .handle(|handle, _id, req| async move {
                    // We got an RxRequest! See what it is.
                    // println!("Got an RxRequest.");
                    match req {
                        RxRequest::Recv(conn_id, packet_data) => {
                            if conn_id == listen_id {
                                // It's a receive on our connection. Grab the incoming buffer and
                                // handle the ping response.
                                let buffer = handle.get_incoming_buffer(packet_data);
                                // println!("Got incoming buffer {:?} from physical layer.", buffer.as_packet_data());
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
            // println!("Done waiting for ping receive requests.");
        })
        .detach();

        // Meanwhile, submit some pings!
        for i in 0..4 {
            // Let's grab a buffer...
            let mut buffer = handle.allocatable_buffer_controller().allocate().await;
            // And fill out that buffer with a ping packet...
            // need a way to uniquely identify this ping instance
            fill_ping_buffer(0, i, &mut buffer);
//            println!("sending ping buffer");
            println!("PING {} ({}) {} bytes of data.", addr.to_string(), addr,buffer.buffer_len());
            // ...and then submit it!
            let _ = handle
                //.submit(TxRequest::Send(listen_id, buffer.as_packet_data()))
                .submit(TxRequest::SendIcmpv4(Ipv4Addr::localhost(),buffer.as_packet_data()) )
                .await;
            Timer::after(Duration::from_millis(1000)).await;
        }
    });
}

fn main() {
    println!("Hello from nettest!");
    let destination = Ipv4Addr::localhost();
    ping(destination);

   /* // Step 1: Get a named networking handle
    let handle = Arc::new(twizzler_net::open_nm_handle("nettest").unwrap());
    println!("nettest got nm handle: {:?}", handle);

    // Step 2: spawn the communication thread 
    twizzler_async::run(async move {
        // Step 3: (can come later) prepare packet to send
        // allocate a buffer in the networking handle
        let mut buffer = handle.allocatable_buffer_controller().allocate().await;
        // copy data into buffer
        buffer.copy_in(b"Some Packet Data");
        // convert it into packet data
        let packet_data = buffer.as_packet_data();
        // Step 4: clone the handle to send to listener thread
        let handle_clone = handle.clone();
        // Step 5: spawn asynchronous listener thread
        // NOTE: This will never terminate
        Task::spawn(async move {
            loop {
                // call the function to handle responses
                // the response handler is defined below in handler()
                let _ = handle_clone.handle(handler).await;
            }
        })
        .detach();
        // Step 6: send the packet data 
        // print result
        let res = handle.submit(TxRequest::Echo(packet_data)).await.unwrap();
        println!("got txc {:?}", res);

        // IPV4 example
        // setup listener task. Reuse handle above.  
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
    */
}

// function to handle responses
// this responds to all messages sent through that handle without checking the communication id
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
