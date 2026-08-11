//! The other end of `net_test`.
//!
//! It exists as a separate program purely so that its sockets live in a *different compartment*,
//! and therefore behind a different `twz-rt` socket engine and a different net-srv client. A
//! single process cannot test connect-meets-accept: both ends would share one engine, one
//! interface and one address.
//!
//! Every mode is a single blocking round trip driven entirely from argv, so `net_test` can treat
//! it as a synchronous step -- spawn, wait for exit, assert. Its address is pinned by
//! `TWZ_NET_ADDR`, which `net_test` sets when spawning; see that crate's `PEER_ADDR`.

use std::{
    env,
    io::{Read, Write},
    net::{TcpStream, UdpSocket},
    process::ExitCode,
    thread::sleep,
    time::Duration,
};

fn usage() -> ! {
    eprintln!(
        "usage: net_test_peer <mode> <addr:port> [arg]\n\
         modes:\n  \
           connect-idle <hold_ms>   connect, then sit there without sending\n  \
           connect-send <msg>       connect, send msg, close\n  \
           connect-echo <msg>       connect, send msg, read the echo back, close\n  \
           connect-drop <linger_ms> connect, send `bye`, drop the stream, then linger\n  \
           connect-n <count>        `count` sequential connections, one at a time\n  \
           udp-send <count>         send `count` datagrams"
    );
    std::process::exit(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        usage();
    }
    let (mode, target) = (args[1].as_str(), args[2].as_str());
    let arg = args.get(3).map(String::as_str).unwrap_or("");

    let result = match mode {
        "connect-idle" => connect_idle(target, arg.parse().unwrap_or(500)),
        "connect-send" => connect_send(target, arg, false),
        "connect-echo" => connect_send(target, arg, true),
        "connect-drop" => connect_drop(target, arg.parse().unwrap_or(2000)),
        "connect-n" => connect_repeatedly(target, arg.parse().unwrap_or(2)),
        "udp-send" => udp_send(target, arg.parse().unwrap_or(4)),
        _ => usage(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("net_test_peer {} {}: {}", mode, target, e);
            ExitCode::FAILURE
        }
    }
}

fn connect_idle(target: &str, hold_ms: u64) -> std::io::Result<()> {
    let stream = TcpStream::connect(target)?;
    // Hold the connection open without sending: the point is a listener that has something for
    // accept() while no data has arrived, which is a state the readiness path used to report as
    // "not readable".
    sleep(Duration::from_millis(hold_ms));
    drop(stream);
    Ok(())
}

fn connect_send(target: &str, msg: &str, expect_echo: bool) -> std::io::Result<()> {
    let mut stream = TcpStream::connect(target)?;
    stream.write_all(msg.as_bytes())?;
    stream.flush()?;
    if expect_echo {
        let mut buf = vec![0u8; msg.len()];
        stream.read_exact(&mut buf)?;
        if buf != msg.as_bytes() {
            return Err(std::io::Error::other(format!(
                "echo mismatch: sent {:?}, got {:?}",
                msg,
                String::from_utf8_lossy(&buf)
            )));
        }
    }
    Ok(())
}

/// Connect, send a marker, then drop the stream *without* calling `shutdown()`.
///
/// The linger afterwards is what keeps this a test of `TcpStreamInner::drop` alone. The FIN that
/// drop queues still needs a poll pass to reach the wire, and nothing drains the engine at
/// compartment exit (the orderly-shutdown item in asyncplan.md); without the linger, a failure
/// would not distinguish "drop emitted no FIN" from "the compartment died before it went out".
fn connect_drop(target: &str, linger_ms: u64) -> std::io::Result<()> {
    let mut stream = TcpStream::connect(target)?;
    stream.write_all(b"bye")?;
    stream.flush()?;
    drop(stream);
    sleep(Duration::from_millis(linger_ms));
    Ok(())
}

fn connect_repeatedly(target: &str, count: usize) -> std::io::Result<()> {
    // Sequential with a gap, not concurrent. The LISTENER-REARM case is specifically "accept one,
    // then get told about the next"; if two connections were pending at once the listener's
    // readiness would never fall, and a correct edge-triggered implementation would rightly stay
    // silent. The gap is what guarantees the server's accept lands between them.
    for i in 0..count {
        if i > 0 {
            sleep(Duration::from_millis(200));
        }
        let mut stream = TcpStream::connect(target)?;
        stream.write_all(format!("conn{}", i).as_bytes())?;
        stream.flush()?;
        // Block until the server closes, rather than closing on a timer. A fixed hold is a race:
        // if it expires before the server's accept runs, the listening socket is no longer active
        // and the accept has nothing to take -- which, with a blocking accept, hangs. Waiting for
        // the server's FIN makes the connection provably alive at accept time and removes the
        // arbitrary sleep.
        let mut buf = [0u8; 8];
        let _ = stream.read(&mut buf);
        drop(stream);
    }
    Ok(())
}

/// Send `count` datagrams, spaced out.
///
/// More than one on purpose. UDP has no retransmit, and smoltcp drops a datagram outright when the
/// destination's link-layer address is not resolved yet -- so the first one to a peer we have not
/// talked to is lost to ARP. A receiver that must see "at least one" is the honest contract; a
/// test that depends on exactly one arriving is testing ARP timing.
///
/// The trailing sleep matters too: `send_to` only queues into smoltcp's transmit buffer, and the
/// engine's polling thread is what puts it on the wire, so exiting immediately would tear the
/// compartment down first.
fn udp_send(target: &str, count: usize) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    for i in 0..count {
        sock.send_to(format!("ping{}", i).as_bytes(), target)?;
        sleep(Duration::from_millis(150));
    }
    Ok(())
}
