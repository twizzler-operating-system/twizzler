// extern crate twizzler_abi;
use tiny_http::shim::SmolTcpListener as TcpListener;
use tiny_http::{shim::SmolTcpStream as TcpStream, Response, Server};
use std::{io::{Read, Write},
        sync::{Arc},
        thread,};

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

    let client = thread::spawn(|| {
        let _ = std_client(9975);
    });

    thread.join().unwrap();
    client.join().unwrap();
}
fn std_client(port: u16) -> std::io::Result<()> {
    println!("in client thread!");
    let mut client = TcpStream::connect(("127.0.0.1", port))?;
    let mut rx_buffer = [0; 2048];
    let msg = b"GET /notes HTTP/1.1\r\n\r\n";
    let _result = client.write(msg)?;
    println!("{}", client.read(&mut rx_buffer)?);
    println!("{}", String::from_utf8((&rx_buffer[0..2048]).to_vec()).unwrap());
    Ok(())
}


