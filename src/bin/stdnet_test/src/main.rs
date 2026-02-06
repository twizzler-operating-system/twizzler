use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
};

const BIG_FILE: &str = "/twizzler-operating-system/twizzler/releases/download/toolchain_13150dd-dea8f77-980d399/toolchain_x86_64_Linux_13150dd-dea8f77-980d399.tar.zst";

fn main() {
    let lookup = "github.com:80".to_socket_addrs().unwrap();
    let first = lookup.into_iter().next();
    println!("got {:?}", first);
    let first = first.unwrap();

    {
        let mut sock = TcpStream::connect(first).unwrap();
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: github.com\r\nUser-Agent: curl/7.1.0\r\nAccept: */*\r\n\r\n",
            BIG_FILE
        );
        sock.write(req.as_bytes()).unwrap();
        sock.shutdown(std::net::Shutdown::Write).unwrap();

        let mut total = 0;
        loop {
            let mut buf = [0; 4096];
            let count = sock.read(&mut buf).unwrap();
            let s = str::from_utf8(&buf[0..count]);
            println!("{} bytes: {:?}", count, s);
            total += count;
            if count == 0 {
                break;
            }
        }

        println!("read {} bytes total", total);
        //let mut v = vec![];
        //sock.read_to_end(&mut v).unwrap();
        //println!("got: {:?}", str::from_utf8(&v));
    }

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
