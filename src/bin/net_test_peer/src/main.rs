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
    os::fd::AsRawFd,
    process::ExitCode,
    thread::sleep,
    time::{Duration, Instant},
};

use twizzler_rt_abi::{
    error::TwzError,
    io::{twz_rt_fd_pread, IoCtx, IoFlags},
};

fn usage() -> ! {
    eprintln!(
        "usage: net_test_peer <mode> <addr:port> [arg]\n\
         modes:\n  \
           connect-idle <hold_ms>   connect, sit there without sending until closed (or hold_ms)\n  \
           connect-send <msg>       connect, send msg, close\n  \
           connect-echo <msg>       connect, send msg, read the echo back, close\n  \
           connect-drop <linger_ms> connect, send `bye`, drop the stream, then linger\n  \
           connect-n <count>        `count` sequential connections, one at a time\n  \
           connect-recv <len>       connect, read `len` bytes of the bulk pattern, expect EOF\n  \
           connect-halfclose <msg>  connect, send msg, shut down writing, read the reply back\n  \
           udp-send <count>         send `count` datagrams\n  \
           udp-echo <attempts>      send pings until one is answered, up to `attempts`"
    );
    std::process::exit(2)
}

/// The byte a bulk transfer carries at offset `i`.
///
/// Position-dependent and not a round power of two, so a duplicated, dropped or reordered run shows
/// up as a mismatch instead of passing on length alone. `net_test` generates the same sequence.
fn bulk_byte(i: usize) -> u8 {
    (i % 251) as u8
}

/// Ceiling on any single bounded read here. Only a backstop against wedging the suite: every one of
/// these reads is answered by the parent in milliseconds when things work.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

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
        "connect-recv" => connect_recv(target, arg.parse().unwrap_or(0)),
        "connect-halfclose" => connect_halfclose(target, arg),
        "udp-send" => udp_send(target, arg.parse().unwrap_or(4)),
        "udp-echo" => udp_echo(target, arg.parse().unwrap_or(20)),
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

/// How often [`wait_for_close`] re-checks. Small enough that the parent's close is not what any
/// test is waiting on, large enough not to matter on a contended single-CPU boot.
const CLOSE_POLL: Duration = Duration::from_millis(20);

/// Hold until the far end closes, giving up after `cap`.
///
/// The parent is done with an idle connection the moment it drops its end, so waiting for EOF
/// rather than a fixed span is what keeps a test's cost its own rather than a timer's -- these
/// holds used to be most of the suite's runtime.
///
/// Nonblocking reads against a deadline, not a blocking `read`: `connect_repeatedly` can afford
/// blocking because its parent always closes, but a mode whose parent may fail an assertion first
/// would hang the whole suite, which has no per-test timeout and cannot use a watchdog thread (see
/// net_test's note). `cap` is exactly the old fixed sleep, so the worst case is unchanged.
///
/// Returns false if `cap` expired with the connection still open.
fn wait_for_close(stream: &TcpStream, cap: Duration) -> bool {
    let fd = stream.as_raw_fd();
    let deadline = Instant::now() + cap;
    while Instant::now() < deadline {
        let mut buf = [0u8; 64];
        let mut ctx = IoCtx::new(None, IoFlags::NONBLOCKING, None);
        match twz_rt_fd_pread(fd, &mut buf, &mut ctx) {
            // EOF: the far end closed, which is the whole signal being waited for.
            Ok(0) => return true,
            // These modes are not sent anything, but a stray byte is no reason to stop waiting.
            Ok(_) => sleep(CLOSE_POLL),
            // Would-block is the steady state here, since the parent sends nothing.
            Err(e) if e == TwzError::WOULD_BLOCK => sleep(CLOSE_POLL),
            // Anything else (a reset, say) means the connection is gone, which answers the same
            // question EOF does. Spinning out `cap` on a dead socket would be the old fixed sleep
            // by another name.
            Err(_) => return true,
        }
    }
    false
}

/// `read_exact`, bounded. False if `cap` expired or the stream ended early.
///
/// Bounded rather than `Read::read_exact` for the reason in `wait_for_close`: a peer blocked
/// forever on data that a bug is withholding takes the whole suite with it.
fn read_exact_within(stream: &TcpStream, buf: &mut [u8], cap: Duration) -> bool {
    let fd = stream.as_raw_fd();
    let deadline = Instant::now() + cap;
    let mut got = 0;
    while got < buf.len() {
        if Instant::now() >= deadline {
            return false;
        }
        let mut ctx = IoCtx::new(None, IoFlags::NONBLOCKING, None);
        match twz_rt_fd_pread(fd, &mut buf[got..], &mut ctx) {
            Ok(0) => return false, // EOF before the whole payload arrived
            Ok(n) => got += n,
            Err(e) if e == TwzError::WOULD_BLOCK => sleep(CLOSE_POLL),
            Err(_) => return false,
        }
    }
    true
}

fn connect_idle(target: &str, hold_ms: u64) -> std::io::Result<()> {
    let stream = TcpStream::connect(target)?;
    // Hold the connection open without sending: the point is a listener that has something for
    // accept() while no data has arrived, which is a state the readiness path used to report as
    // "not readable". Reading does not send, so the connection stays as silent as a fixed sleep
    // left it.
    //
    // Failing on the cap rather than returning Ok is what makes every connect-idle test a check
    // that closing really delivers a FIN. The parent always drops its end before waiting on us, so
    // reaching the cap means the EOF never arrived -- which is a bug, not a slow machine, and used
    // to show up only as a test that took its whole hold. See `listener_socket_ready` in twz-rt.
    if !wait_for_close(&stream, Duration::from_millis(hold_ms)) {
        return Err(std::io::Error::other(format!(
            "no EOF within {}ms of the parent closing",
            hold_ms
        )));
    }
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
            // Short, because the read below already provides the ordering this gap was carrying:
            // the previous connection is not released until the server closed it, so the accept
            // provably landed before the next SYN. What is left is margin on the listener's
            // readiness falling, not the accept itself.
            sleep(Duration::from_millis(50));
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

/// Read `len` bytes of the bulk pattern, then require the stream to end exactly there.
///
/// The trailing EOF check is half the point: a transfer that arrives intact but keeps delivering
/// has lost its framing, and length alone would not catch it.
fn connect_recv(target: &str, len: usize) -> std::io::Result<()> {
    let stream = TcpStream::connect(target)?;
    let mut buf = vec![0u8; len];
    if !read_exact_within(&stream, &mut buf, READ_TIMEOUT) {
        return Err(std::io::Error::other(format!(
            "only part of the {}-byte transfer arrived within {:?}",
            len, READ_TIMEOUT
        )));
    }
    if let Some(i) = (0..len).find(|&i| buf[i] != bulk_byte(i)) {
        return Err(std::io::Error::other(format!(
            "byte {} is {}, expected {}",
            i,
            buf[i],
            bulk_byte(i)
        )));
    }
    if !wait_for_close(&stream, READ_TIMEOUT) {
        return Err(std::io::Error::other("no EOF after the transfer"));
    }
    Ok(())
}

/// Send `msg`, shut down the write half, and require the reply to still come back.
///
/// This is the half-close shape every request/response protocol over TCP uses to say "that is the
/// whole request": shutting down writing must deliver EOF to the far end without tearing down the
/// direction it answers on.
fn connect_halfclose(target: &str, msg: &str) -> std::io::Result<()> {
    let mut stream = TcpStream::connect(target)?;
    stream.write_all(msg.as_bytes())?;
    stream.flush()?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut buf = vec![0u8; msg.len()];
    if !read_exact_within(&stream, &mut buf, READ_TIMEOUT) {
        return Err(std::io::Error::other(
            "no reply after shutting down the write half",
        ));
    }
    if buf != msg.as_bytes() {
        return Err(std::io::Error::other(format!(
            "half-close reply mismatch: sent {:?}, got {:?}",
            msg,
            String::from_utf8_lossy(&buf)
        )));
    }
    Ok(())
}

/// Ping until one is answered, up to `attempts`.
///
/// Retrying is what makes this a test of the round trip rather than of packet luck: either
/// direction can lose a datagram to ARP or to a receiver with no free rx packet, and a single
/// exchange would fail on that alone. One answer is proof enough that the parent saw our address
/// correctly in `recv_from` and could send back to it.
///
/// Binding our own address rather than the wildcard is not a style choice: a `0.0.0.0` UDP bind
/// currently receives nothing (`udp::Socket::accepts` rejects every datagram whose destination is
/// not literally 0.0.0.0), so a wildcard bind here would never see the reply.
fn udp_echo(target: &str, attempts: usize) -> std::io::Result<()> {
    let self_addr = env::var("TWZ_NET_ADDR").unwrap_or_else(|_| "0.0.0.0".to_string());
    let sock = UdpSocket::bind(format!("{}:0", self_addr))?;
    let fd = sock.as_raw_fd();

    for i in 0..attempts {
        sock.send_to(format!("ping{}", i).as_bytes(), target)?;

        // Wait out the gap between sends looking for a reply, rather than sleeping and then
        // checking: the answer usually lands within a millisecond or two of the first ping.
        let deadline = Instant::now() + Duration::from_millis(150);
        while Instant::now() < deadline {
            let mut buf = [0u8; 64];
            let mut ctx = IoCtx::new(None, IoFlags::NONBLOCKING, None);
            match twz_rt_fd_pread(fd, &mut buf, &mut ctx) {
                Ok(n) if buf[..n].starts_with(b"pong") => return Ok(()),
                // Some other datagram; keep waiting for ours.
                Ok(_) => {}
                Err(e) if e == TwzError::WOULD_BLOCK => sleep(CLOSE_POLL),
                Err(e) => return Err(std::io::Error::other(format!("udp recv: {}", e))),
            }
        }
    }
    Err(std::io::Error::other(format!(
        "no reply to any of {} pings",
        attempts
    )))
}
