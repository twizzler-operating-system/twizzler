// extern crate twizzler_abi;
use std::sync::{Arc, Mutex};
use tiny_http::{Server, Response};
// #[path="./tiny-http-twizzler/src/shim.rs"] 
// mod shim;

use tiny_http::shim::SmolTcpStream as TcpStream;
use tiny_http::shim::SmolTcpListener as TcpListener;

// hello world made single threaded : TINY_HTTP
fn main() {
    // main
    let listener = {
        println!("in main!");
        TcpListener::bind("127.0.0.1:1234").unwrap()
    };
    println!("bound to 127.0.0.1:1234");
    // server thread
    let server = std::thread::spawn(move || {
        match listener.accept() {
            Ok((_socket, addr)) => println!("new client: {addr:?}"),
            Err(e) => println!("couldn't get client: {e:?}"),
        }
        println!("accepted connection");
    });

    // client thread
    let client = std::thread::spawn (move || {
        std_client(1234);
    });
    server.join().unwrap();
    client.join().unwrap();
}

fn std_server(){
    let listener = {
        let m = Arc::new(Mutex::new(0)); // unlocked state
        m.lock().unwrap();
        println!("in standard server thread!");
        println!("before bind()");
        TcpListener::bind("127.0.0.1:1234").unwrap()
    };
    println!("after bind()");
    // block to drop mutex after
    match listener.accept() {
        Ok((_socket, addr)) => println!("new client: {addr:?}"),
        Err(e) => println!("couldn't get client: {e:?}"),
    }
    println!("accepted connection");
}
// tiny-http server
fn tiny_http_server(){
    println!("in tiny_http server thread!");
    let server = Arc::new(Server::http("0.0.0.0:1234").unwrap());
    println!("server: now listening on port 1234");
    for rq in server.incoming_requests() {
        let response = Response::from_string("hello world".to_string());
        let _ = rq.respond(response);
    }
}
// std client
fn std_client(port: u16) {
    println!("in client thread!");
    if let client = TcpStream::connect(("127.0.0.1",port)).unwrap() {
        println!("Connected to the server!");
    } else {
        println!("Couldn't connect to server...");
    }
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// smoltcp "server"
fn smoltcp_server(){
    println!("in server thread!");

    // creating my own lil smoltcp server
    let tcp1_rx_buffer = tcp::SocketBuffer::new(vec![0; 64]);
    let tcp1_tx_buffer = tcp::SocketBuffer::new(vec![0; 128]);
    let tcp1_socket = tcp::Socket::new(tcp1_rx_buffer, tcp1_tx_buffer);
    let mut sockets = SocketSet::new(vec![]);
    let tcp1_handle = sockets.add(tcp1_socket);
    // let mut tcp_6970_active = false;
    // tcp:6969: respond "hello"
    let socket = sockets.get_mut::<tcp::Socket>(tcp1_handle);
    if !socket.is_open() {
        socket.listen(1234).unwrap();
        println!("server: state: {}", socket.state());
    }
    if socket.can_send() {
        println!("server: tcp:1234 send greeting");
        writeln!(socket, "hello").unwrap();
        println!("server: sent hello");
        println!("server: tcp:1234 close");
        socket.close();
    }
}

// smoltcp client
use std::net::{Ipv4Addr, IpAddr};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Loopback, Medium};
use smoltcp::wire::{EthernetAddress, IpEndpoint, IpAddress, IpCidr};
use std::fmt::Write;
pub type SocketBuffer<'a> = smoltcp::storage::RingBuffer<'a, u8>;

fn smoltcp_client() {
    println!("in client thread!");
    // open tcp socket
    let rx_buffer = SocketBuffer::new(Vec::new());
    let tx_buffer = SocketBuffer::new(Vec::new());
    let mut sock = tcp::Socket::new(rx_buffer, tx_buffer);
    let config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into()); // change later?
    let mut device = Loopback::new(Medium::Ethernet);
    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
            .unwrap();
    });
    let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let error = sock.connect(iface.context(), (addr, 1234), 49152); // make sure local endpoint matches the server address
    match error {
        Err(e) => {
            println!("connection error!! {}", e);
            return;
        },
        Ok(()) => {println!("ok");},
    }
    println!("local_endpoint: {}", sock.local_endpoint().unwrap());
    println!("remote_endpoint: {}", sock.remote_endpoint().unwrap());
    // write a single static string for http req to it
    let request = "GET /notes HTTP/1.1\r\n\r\n";
    println!("client state: {}", sock.state());
    if sock.may_send() {
        sock.send_slice(request.as_ref()).expect("cannot send");
        println!("sent req!");
        // close connection
        sock.send_slice(b"Connection: close\r\n").expect("cannot send");
        sock.send_slice(b"\r\n").expect("cannot send");
    }
    if sock.may_recv() {
        sock.recv(|data| {
            println!("{}", std::str::from_utf8(data).unwrap_or("(invalid utf8)"));
            (data.len(), ())
        }).unwrap();
    }
    // receive whatever
    // check that it received a response
    // close it.
}
