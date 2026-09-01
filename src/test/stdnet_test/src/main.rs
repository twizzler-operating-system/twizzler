#![allow(unreachable_code, dead_code, unused_imports)]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    time::Instant,
};

use async_executor::LocalExecutor;

mod async_test {
    use std::net::SocketAddr;

    use async_net::TcpStream;
    use futures_lite::{AsyncReadExt, AsyncWriteExt};

    pub async fn async_stdnet_connect_test(addr: SocketAddr) {
        let mut sock = TcpStream::connect(addr).await.unwrap();

        let req = format!(
            "GET / HTTP/1.1\r\nHost: google.com\r\nUser-Agent: curl/7.1.0\r\nAccept: */*\r\n\r\n",
        );
        sock.write(req.as_bytes()).await.unwrap();
        sock.shutdown(std::net::Shutdown::Write).unwrap();
        let mut total = 0;
        loop {
            let mut buf = [0; 4096];
            let count = sock.read(&mut buf).await.unwrap();
            if total == 0 {
                let s = str::from_utf8(&buf[0..count.min(256)]);
                println!("{} bytes: {:?}", count, s);
            }
            total += count;
            if count == 0 || total == 104857600 {
                break;
            }
        }
    }
}

fn main() {
    {
        println!("doing async connect test");
        let lookup = "google.com:80".to_socket_addrs().unwrap();
        let first = lookup.into_iter().next();
        println!("got {:?}", first);
        let first = first.unwrap();
        let exec = LocalExecutor::new();
        async_io::block_on(exec.run(async { async_test::async_stdnet_connect_test(first).await }));
        return;
    }

    {
        let lookup = "ash-speed.hetzner.com:80".to_socket_addrs().unwrap();
        let first = lookup.into_iter().next();
        println!("got {:?}", first);
        let first = first.unwrap();
        const BIG_FILE: &str = "/100MB.bin";
        let mut sock = TcpStream::connect(first).unwrap();
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: ash-speed.hetzner.com\r\nUser-Agent: curl/7.1.0\r\nAccept: */*\r\n\r\n",
            BIG_FILE
        );
        sock.write(req.as_bytes()).unwrap();

        let mut total = 0;
        let start = Instant::now();
        loop {
            let mut buf = [0; 4096];
            let count = sock.read(&mut buf).unwrap();
            if total == 0 {
                let s = str::from_utf8(&buf[0..count.min(256)]);
                println!("{} bytes: {:?}", count, s);
            } else {
                let speed = (total as f64 / start.elapsed().as_millis() as f64) * 1000.0;
                println!(
                    "{} bytes ({} / {}): {:.2}KB/s",
                    count,
                    total,
                    104857600,
                    speed / 1024.0
                );
            }
            total += count;
            if count == 0 || total == 104857600 {
                break;
            }
        }
        sock.shutdown(std::net::Shutdown::Write).unwrap();

        println!("read {} bytes total ({}MB)", total, total / (1024 * 1024));
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
