// extern crate twizzler_abi;
// #[path="./tiny-http-twizzler/src/shim.rs"]
// mod shim;
use tiny_http::shim::SmolTcpListener as TcpListener;
use tiny_http::{shim::SmolTcpStream as TcpStream, Response, Server};
use std::{io::{Read, Write},
        sync::{Arc, Mutex},
        thread,};
use std::net::Shutdown;

// hello world made single threaded : TINY_HTTP
fn main() {
    let server = Arc::new(Server::http("127.0.0.1:9975").unwrap());
    println!("Now listening on port 9975");

    let thread = thread::spawn(move || {
        for request in server.incoming_requests() {
            println!(
                "received request! method: {:?}, url: {:?}, headers: {:?}",
                request.method(),
                request.url(),
                request.headers()
            );
    
            let response = Response::from_string("hello world");
            request.respond(response).expect("Responded");
        }
    });

    let client = thread::spawn(move || {
        let _ = std_client(9975);
    });

    thread.join().unwrap();
    client.join().unwrap();
}
fn std_client(port: u16) -> std::io::Result<()> {
    println!("in client thread!");
    thread::sleep(std::time::Duration::from_millis(2000));
    let mut client = TcpStream::connect(("127.0.0.1", port))?;
    let mut rx_buffer = [0; 2048];
    let msg = b"GET /notes HTTP/1.1\r\n\r\n";
    let _result = client.write(msg)?;
    thread::sleep(std::time::Duration::from_millis(2000));
    let _bytes_read = client.read(&mut rx_buffer)?;
    println!("{}", String::from_utf8((&rx_buffer[0..2048]).to_vec()).unwrap());
    Ok(())
}

fn handle_connection(mut stream: (TcpStream, std::net::SocketAddr)) {
    let mut stream1 = stream.0;
    let mut buffer = [0; 512];
    stream1.read(&mut buffer).unwrap();
    println!("Request: {}", String::from_utf8_lossy(&buffer[..]));
    stream1.shutdown(Shutdown::Write);
}

pub fn create_listener(listener: TcpListener) -> std::io::Result<()> {
    let stream = listener.accept().unwrap();
    handle_connection(stream);
    Ok(())
}

// OLD std client
// fn std_client(port: u16) {
//     println!("in client thread!");
//     let client = TcpStream::connect(("127.0.0.1", port));
//     if let Ok(SmolTcpStream) = client {
//         println!("Connected to the server! {} {}", "127.0.0.1", port);
//     } else {
//         println!("Couldn't connect to server...");
//     }
// }

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// smoltcp "server"
// fn smoltcp_server() {
//     println!("in server thread!");

//     // creating my own lil smoltcp server
//     let tcp1_rx_buffer = tcp::SocketBuffer::new(vec![0; 64]);
//     let tcp1_tx_buffer = tcp::SocketBuffer::new(vec![0; 128]);
//     let tcp1_socket = tcp::Socket::new(tcp1_rx_buffer, tcp1_tx_buffer);
//     let mut sockets = SocketSet::new(vec![]);
//     let tcp1_handle = sockets.add(tcp1_socket);
//     // let mut tcp_6970_active = false;
//     // tcp:6969: respond "hello"
//     let socket = sockets.get_mut::<tcp::Socket>(tcp1_handle);
//     if !socket.is_open() {
//         socket.listen(1234).unwrap();
//         println!("server: state: {}", socket.state());
//     }
//     if socket.can_send() {
//         println!("server: tcp:1234 send greeting");
//         writeln!(socket, "hello").unwrap();
//         println!("server: sent hello");
//         println!("server: tcp:1234 close");
//         socket.close();
//     }
// }

// // smoltcp client
// use std::{
//     fmt::Write,
//     net::{IpAddr, Ipv4Addr},
// };

// use smoltcp::{
//     iface::{Config, Interface, SocketSet},
//     phy::{Loopback, Medium},
//     socket::tcp,
//     time::Instant,
//     wire::{EthernetAddress, IpAddress, IpCidr, IpEndpoint},
// };
// pub type SocketBuffer<'a> = smoltcp::storage::RingBuffer<'a, u8>;

// fn smoltcp_client() {
//     println!("in client thread!");
//     // open tcp socket
//     let rx_buffer = SocketBuffer::new(Vec::new());
//     let tx_buffer = SocketBuffer::new(Vec::new());
//     let mut sock = tcp::Socket::new(rx_buffer, tx_buffer);
//     let config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into()); // change later?
//     let mut device = Loopback::new(Medium::Ethernet);
//     let mut iface = Interface::new(config, &mut device, Instant::now());
//     iface.update_ip_addrs(|ip_addrs| {
//         ip_addrs
//             .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
//             .unwrap();
//     });
//     let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
//     let error = sock.connect(iface.context(), (addr, 1234), 49152); // make sure local endpoint matches the server address
//     match error {
//         Err(e) => {
//             println!("connection error!! {}", e);
//             return;
//         }
//         Ok(()) => {
//             println!("ok");
//         }
//     }
//     println!("local_endpoint: {}", sock.local_endpoint().unwrap());
//     println!("remote_endpoint: {}", sock.remote_endpoint().unwrap());
//     // write a single static string for http req to it
//     let request = "GET /notes HTTP/1.1\r\n\r\n";
//     println!("client state: {}", sock.state());
//     if sock.may_send() {
//         sock.send_slice(request.as_ref()).expect("cannot send");
//         println!("sent req!");
//         // close connection
//         sock.send_slice(b"Connection: close\r\n")
//             .expect("cannot send");
//         sock.send_slice(b"\r\n").expect("cannot send");
//     }
//     if sock.may_recv() {
//         sock.recv(|data| {
//             println!("{}", std::str::from_utf8(data).unwrap_or("(invalid utf8)"));
//             (data.len(), ())
//         })
//         .unwrap();
//     }
//     // receive whatever
//     // check that it received a response
//     // close it.
// }
