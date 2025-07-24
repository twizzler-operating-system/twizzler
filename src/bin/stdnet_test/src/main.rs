use std::net::TcpListener;
use std::io::{Read, Write};

fn main() {
    let listener = TcpListener::bind("0.0.0.0:5555").expect("bind failed");
    println!("Listening on 0.0.0.0:5555. Waiting for a client...");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                println!("Client connected: {:?}", stream.peer_addr());
                let mut buf = [0u8; 512];
                loop {
                    let n = match stream.read(&mut buf) {
                        Ok(0) => {
                            println!("Client disconnected.");
                            break;
                        }
                        Ok(n) => n,
                        Err(e) => {
                            eprintln!("Read error: {}", e);
                            break;
                        }
                    };
                    stream.write_all(&buf[..n]).expect("write failed");
                }
            }
            Err(e) => {
                std::thread::sleep(std::time::Duration::from_secs(1));
                eprintln!("Bind failed with error: {:?}", e);
                eprintln!("Error kind: {:?}", e.kind());
                eprintln!("Error message: {}", e);
            
            }
        }
    }
}