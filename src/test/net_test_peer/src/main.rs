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
    net::{TcpListener, TcpStream, UdpSocket},
    os::fd::AsRawFd,
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
    thread::sleep,
    time::{Duration, Instant},
};

use twizzler_rt_abi::{
    bindings::{
        kevent, option_duration, twz_rt_fd_kevent, EVFILT_READ, EVFILT_WRITE, EV_ADD, EV_ERROR,
        EV_RECEIPT,
    },
    error::TwzError,
    fd::RawFd,
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
           udp-echo <attempts>      send pings until one is answered, up to `attempts`\n  \
           serve-echo <idle_ms>     listen, accept one connection, echo until EOF (benchmarks)\n  \
           serve-udp-echo <idle_ms> listen, echo datagrams back until idle for `idle_ms`"
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
        "serve-echo" => serve_echo(target, arg.parse().unwrap_or(20_000)),
        "serve-udp-echo" => serve_udp_echo(target, arg.parse().unwrap_or(20_000)),
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

// ---------------------------------------------------------------------------------------------
// Long-lived echo servers, for the sysbench network benchmarks.
//
// Every other mode here is a single round trip, because `net_test` grades a peer by exit status.
// These two are the exception: a throughput or latency number needs many round trips over one
// established connection, so the peer has to outlive a single exchange. They still terminate on
// their own -- on EOF for TCP, on an idle deadline for UDP -- so a benchmark that dies does not
// leave a compartment running.

/// Readiness wait, so neither `accept()` nor `recv_from()` can block forever.
///
/// Same reasoning as `net_test`'s `accept_within`: nothing imposes a timeout on a peer, so an
/// unbounded block here would wedge the whole suite rather than fail one benchmark. Level
/// triggered (no `EV_CLEAR`), so an already-pending connection or datagram still reports.
static BACKOFF_HITS: AtomicU64 = AtomicU64::new(0);
static WR_REGISTERED: AtomicU64 = AtomicU64::new(0);
static WR_REGFAIL: AtomicU64 = AtomicU64::new(0);
static WR_NOTREADY: AtomicU64 = AtomicU64::new(0);

fn ev(ident: RawFd, filter: i16, flags: u16) -> kevent {
    kevent {
        ident: ident as usize,
        filter,
        flags,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
        ext: [0; 4],
    }
}

fn kev_call(kq: RawFd, chg: &[kevent], out: &mut [kevent], t: Option<Duration>) -> usize {
    let t: option_duration = t.into();
    let res =
        unsafe { twz_rt_fd_kevent(kq, chg.as_ptr(), chg.len(), out.as_mut_ptr(), out.len(), t) };
    let r: twizzler_rt_abi::Result<usize> = res.into();
    r.expect("kevent")
}

/// A kqueue armed once and reused across waits — the point of kqueue. The open/register/close-
/// per-call pattern in `wait_ready` costs a kqueue fd, a registration (which republishes
/// readiness into the engine), and a teardown *per datagram* on the UDP echo path.
///
/// Registration keeps `wait_ready`'s rule: the receipt is *identified* among the returned events
/// (an already-readable fd yields its readiness event from the same call). The registration is
/// level-triggered (no EV_CLEAR), so readiness delivered alongside the receipt is not lost — the
/// next wait re-reports it.
struct ArmedWait {
    kq: RawFd,
    fd: RawFd,
    filter: i16,
}

impl ArmedWait {
    fn new(fd: RawFd, filter: i16) -> Option<Self> {
        let kq = twizzler_rt_abi::fd::twz_rt_fd_open_kqueue(0).ok()?;
        let mut out = [ev(0, 0, 0); 4];
        let n = kev_call(
            kq,
            &[ev(fd, filter, EV_ADD | EV_RECEIPT)],
            &mut out,
            Some(Duration::ZERO),
        );
        let registered = out[..n.min(out.len())]
            .iter()
            .any(|e| e.flags & EV_ERROR == EV_ERROR && e.data == 0);
        if !registered {
            println!("WAITREG armed registration did not take (n={})", n);
            twizzler_rt_abi::fd::twz_rt_fd_close(kq);
            return None;
        }
        Some(Self { kq, fd, filter })
    }

    /// One readiness wait. Identity, not count: an EV_ERROR or another registration's event is
    /// not readiness on this fd.
    fn wait(&self, timeout: Duration) -> bool {
        let mut out = [ev(0, 0, 0); 4];
        let n = kev_call(self.kq, &[], &mut out, Some(timeout));
        out[..n.min(out.len())].iter().any(|e| {
            e.ident == self.fd as usize && e.filter == self.filter && e.flags & EV_ERROR == 0
        })
    }
}

impl Drop for ArmedWait {
    fn drop(&mut self) {
        twizzler_rt_abi::fd::twz_rt_fd_close(self.kq);
    }
}

fn wait_ready(fd: RawFd, filter: i16, timeout: Duration) -> bool {
    let call = kev_call;

    let Ok(kq) = twizzler_rt_abi::fd::twz_rt_fd_open_kqueue(0) else {
        return false;
    };
    let mut out = [ev(0, 0, 0); 4];
    // EV_RECEIPT forces the registration to return rather than going on to wait, so the wait
    // below is the only thing that consumes readiness.
    let n = call(
        kq,
        &[ev(fd, filter, EV_ADD | EV_RECEIPT)],
        &mut out,
        Some(Duration::ZERO),
    );
    // `n == 1` was wrong, and wrong precisely when the socket has data.
    //
    // EV_RECEIPT makes the registration call return without waiting, so it yields the receipt --
    // but if the fd is *already* readable the same call also returns the readiness event, giving
    // n == 2. The old check read that as a failed registration, short-circuited `ready` to false
    // without ever consulting the level, and reported "not ready" for a socket holding data. That
    // is the whole bug: measured, a plain read taken immediately after a 20s not-ready returned 64
    // bytes, and `registered-but-notready` never once fired -- the level check was always right
    // when it was allowed to run.
    //
    // The receipt is the event matching the change we submitted (EV_ERROR set, data == 0 meaning
    // success); anything else returned alongside it is real readiness, not a problem.
    let registered = out[..n.min(out.len())]
        .iter()
        .any(|e| e.flags & EV_ERROR == EV_ERROR && e.data == 0);
    // `ready` is false when registration fails, WITHOUT the level ever being consulted -- so a
    // registration that quietly does not take is indistinguishable from "no data", which is
    // exactly the shape of this bug (a read straight after a 20s not-ready returns 64 bytes).
    // Count the two outcomes separately so they can never be confused again.
    if registered {
        WR_REGISTERED.fetch_add(1, Ordering::Relaxed);
    } else {
        let c = WR_REGFAIL.fetch_add(1, Ordering::Relaxed) + 1;
        if c.is_power_of_two() {
            println!(
                "WAITREG fail n={} ret={} flags={:#x} data={} (registration did not take)",
                c, n, out[0].flags, out[0].data
            );
        }
    }
    // Same rule as the receipt above: identify the event, do not count events. `> 0` would accept
    // an EV_ERROR, or an event for some other registration, as readiness on this fd.
    let ready = registered && {
        let n2 = call(kq, &[], &mut out, Some(timeout));
        out[..n2.min(out.len())]
            .iter()
            .any(|e| e.ident == fd as usize && e.filter == filter && e.flags & EV_ERROR == 0)
    };
    if registered && !ready {
        let c = WR_NOTREADY.fetch_add(1, Ordering::Relaxed) + 1;
        if c.is_power_of_two() {
            println!("WAITREG registered-but-notready n={}", c);
        }
    }
    twizzler_rt_abi::fd::twz_rt_fd_close(kq);
    ready
}

fn wait_readable(fd: RawFd, timeout: Duration) -> bool {
    wait_ready(fd, EVFILT_READ, timeout)
}

fn wait_writable(fd: RawFd, timeout: Duration) -> bool {
    wait_ready(fd, EVFILT_WRITE, timeout)
}

/// `wait_readable` bounded by a measured deadline rather than one intended timeout.
///
/// `wait_ready` returns false for "no events", and a kevent call can return early with none --
/// KEVSHORT records 20000ms requests returning in 39us. A single such return is indistinguishable
/// from a genuine timeout, so treating one as the whole idle period retires the peer roughly 20s
/// early. `serve_echo` already slices against a real deadline for exactly this reason; this is the
/// same fix for the datagram path, which was left on the single-shot wait.
/// Monotonic ns anchored to the kernel clock at FastClock calibration, so values are comparable
/// across compartments — the latency-decomposition stamps rely on that.
fn mono_ns() -> u64 {
    twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64
}

fn wait_readable_until(rw: &ArmedWait, idle: Duration) -> bool {
    let slice = Duration::from_secs(1);
    let deadline = Instant::now() + idle;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let t0 = Instant::now();
        if rw.wait(remaining.min(slice)) {
            return true;
        }
        // Back off when the wait returned early. `wait_ready` cannot distinguish "timed out" from
        // "returned with no events", and KEVSHORT records 20000ms requests coming back in 39us --
        // so retrying immediately is a hot spin, not a wait. The first version of this loop did
        // exactly that for a full 20s: every iteration opens a kqueue and re-arms, and each arm
        // calls `refresh_waiter`, which published readiness 1.4 million times in one run and
        // starved the engine it was waiting on. Only sleep when the call came back short; a wait
        // that genuinely consumed its slice needs no delay.
        let elapsed = t0.elapsed();
        if elapsed < Duration::from_millis(1) {
            // Counted: twizzler-56 observes net_udp_latency's ~1.01ms RTT matching this 1ms
            // backoff exactly, and if it fires once per echo it *is* the measurement. It may now
            // be dead code -- the registration bug made `wait_ready` return instantly without ever
            // blocking, which is precisely what drove this path -- so measure before tuning it.
            let c = BACKOFF_HITS.fetch_add(1, Ordering::Relaxed) + 1;
            if c.is_power_of_two() {
                println!(
                    "WAITBACKOFF hits={} last_elapsed_us={}",
                    c,
                    elapsed.as_micros()
                );
            }
            std::thread::sleep(Duration::from_millis(1) - elapsed);
        }
    }
}

/// Largest single echo chunk. Sized above the benchmark's block so a bulk transfer is not
/// artificially split by this buffer.
const ECHO_BUF: usize = 128 * 1024;

/// Accept one connection on `listen` and echo every byte back until the client closes.
///
/// Terminates on EOF, so the benchmark ends this peer by dropping its stream. `idle_ms` bounds
/// both the wait for a connection and any single stall mid-stream.
fn serve_echo(listen: &str, idle_ms: u64) -> std::io::Result<()> {
    let idle = Duration::from_millis(idle_ms);
    let listener = TcpListener::bind(listen)?;
    // Sliced, not one long wait. Identical total bound, but each slice re-enters the runtime from
    // this thread -- the only one still running when the engine stops -- so engine liveness gets
    // sampled from outside the thing being measured.
    // Bounded by a deadline, not by accumulated intended slices. `waited += slice` added a full
    // second per iteration however long the wait actually took, so a `wait_readable` returning
    // early -- for any reason -- made twenty ~2.5ms polls report a twenty-second timeout. That
    // fabricated duration sat upstream of every other measurement here: the peer exited after
    // ~50ms while its own engine was still initialising, and the "20s hang" it reported sent an
    // entire investigation looking for a stuck thread that never existed.
    let slice = Duration::from_secs(1);
    let start = Instant::now();
    let deadline = start + idle;
    let mut got = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        // Still sliced, for the reason above, but each slice is clamped to what is actually left
        // so slicing can never overshoot or undershoot the real bound.
        if wait_readable(listener.as_raw_fd(), remaining.min(slice)) {
            got = true;
            break;
        }
    }
    if !got {
        // Report the *measured* elapsed time, not the requested bound. A timeout message that
        // states its own parameter can only ever agree with itself; had this printed 48ms next
        // to a 20s request, the defect would have been visible the first time it fired.
        // Name the listener. A round runs four net benches across ~20 compartments and this
        // message appeared without an address or port, so a single failure could not be tied to
        // the compartment that produced it -- every per-compartment trace had to be matched by
        // guessing which one had a ~20s engine. `listen` is "10.0.2.N:PORT" and has been in
        // scope the whole time.
        return Err(std::io::Error::other(format!(
            "no connection on {}: waited {:?} of {:?}",
            listen,
            start.elapsed(),
            idle
        )));
    }
    let (mut stream, _) = listener.accept()?;
    let fd = stream.as_raw_fd();
    let mut buf = vec![0u8; ECHO_BUF];
    let mut echoed: usize = 0;

    loop {
        let deadline = Instant::now() + idle;
        let n = loop {
            let mut ctx = IoCtx::new(None, IoFlags::NONBLOCKING, None);
            match twz_rt_fd_pread(fd, &mut buf, &mut ctx) {
                Ok(n) => break n,
                Err(e) if e == TwzError::WOULD_BLOCK => {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::other(format!("idle for {:?}", idle)));
                    }
                    // Yield rather than spin. A benchmark's next request is microseconds away so
                    // sleeping would land in the measurement -- but a pure `spin_loop` pins a
                    // vcpu, and this guest has four and roughly a dozen compartments. Starving
                    // net-srv's device thread while waiting for a frame *it* has to deliver is a
                    // deadlock built out of scheduling rather than buffers.
                    std::thread::yield_now();
                }
                // A reset ends the stream as definitively as EOF does.
                //
                // Reporting the reason, not just returning: every `Ok(())` here exits the peer
                // silently, and the parent then reads "peer exited mid-benchmark" with no cause.
                // Seven of nine failures in netarm-t1 left no message at all, so the dominant
                // failure mode of this whole investigation has been invisible by construction.
                Err(e) => {
                    eprintln!(
                        "serve-echo EXIT: read error after {} bytes: {:?}",
                        echoed, e
                    );
                    return Ok(());
                }
            }
        };
        if n == 0 {
            eprintln!("serve-echo EXIT: EOF after {} bytes echoed", echoed);
            return Ok(());
        }
        echoed += n;
        // Same backpressure rule as the UDP path, but a stream cannot drop bytes: wait for
        // writability and finish the write, and only give up when the peer has gone quiet for
        // the whole idle bound.
        let mut off = 0usize;
        while off < n {
            match stream.write(&buf[off..n]) {
                Ok(0) => {
                    eprintln!("serve-echo EXIT: write returned 0 after {} bytes", echoed);
                    return Ok(());
                }
                Ok(w) => off += w,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if !wait_writable(fd, idle) {
                        return Err(std::io::Error::other(format!(
                            "peer stopped reading for {:?} with {} bytes left",
                            idle,
                            n - off
                        )));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Echo datagrams back to their sender until nothing arrives for `idle_ms`.
///
/// UDP has no EOF, so an idle deadline is the only termination this can have. The benchmark
/// simply stops sending and lets it expire.
fn serve_udp_echo(listen: &str, idle_ms: u64) -> std::io::Result<()> {
    let idle = Duration::from_millis(idle_ms);
    let sock = UdpSocket::bind(listen)?;
    let fd = sock.as_raw_fd();
    // Non-blocking so the post-readiness drain below can end on WouldBlock rather than parking
    // again. `wait_readable_until` still provides the blocking behaviour; this only changes how
    // the loop discovers it has emptied the socket.
    sock.set_nonblocking(true)?;
    // Armed once, reused for every wait below; see ArmedWait.
    let rw = ArmedWait::new(fd, EVFILT_READ)
        .ok_or_else(|| std::io::Error::other("kqueue registration failed"))?;
    if SPIN_PROBE {
        match sock.set_nonblocking(true) {
            Ok(()) => println!("UDPECHO nonblocking=ok"),
            Err(e) => println!("UDPECHO nonblocking=FAILED {:?}", e),
        }
    }
    let mut buf = vec![0u8; 64 * 1024];

    // Diagnostics: the failure here has survived three rounds of inference, so count what the
    // loop actually does rather than deducing it from engine counters that instrument a
    // different code path.
    // Probe retired: it established (netprobe2) that datagrams are queued and reachable all along
    // -- the peer echoed 18,902 of them and the bench reported 0 timeouts of 18,901 -- so the
    // defect was never delivery, it was the readiness path failing to wake a parked waiter.
    // Back on the kevent path, which is what the fix has to make work.
    const SPIN_PROBE: bool = false;
    let (mut recvs, mut sends, mut wb, mut wwfail, mut notready) = (0u64, 0u64, 0u64, 0u64, 0u64);
    // Readiness under-report tripwire; see the drain loop below.
    let (mut multidrain, mut maxdrain) = (0u64, 0usize);
    let mut spins = 0u64;
    let mut report = std::time::Instant::now();
    loop {
        if report.elapsed() >= Duration::from_secs(2) {
            report = std::time::Instant::now();
            println!(
                "UDPECHO recv={} sent={} wouldblock={} waitwrite_timeout={} notready={} multidrain={} maxdrain={}",
                recvs, sends, wb, wwfail, notready, multidrain, maxdrain
            );
        }
        // PROBE (netprobe1): does the data reach the socket at all, or is only the readiness
        // path broken? Spin on a non-blocking recv instead of parking in `wait_readable`. A
        // recv is a socket call, so this also wakes our own poll thread -- which is the point:
        // if datagrams appear now, they were queued and reachable all along and the kevent
        // readiness path is what fails. If they still do not appear, the data never arrived.
        if SPIN_PROBE {
            let deadline = std::time::Instant::now() + idle;
            let mut got = None;
            while std::time::Instant::now() < deadline {
                match sock.recv_from(&mut buf) {
                    Ok(v) => {
                        got = Some(v);
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        spins += 1;
                        std::thread::yield_now();
                    }
                    Err(e) => return Err(e),
                }
            }
            let Some((n, from)) = got else {
                println!(
                    "UDPECHO exit-idle-spin recv={} sent={} spins={} wouldblock={}",
                    recvs, sends, spins, wb
                );
                return Ok(());
            };
            recvs += 1;
            let _ = sock.send_to(&buf[..n], from);
            sends += 1;
            continue;
        }
        let t_wait_ret;
        let idle_start = Instant::now();
        if !wait_readable_until(&rw, idle) {
            notready += 1;
            // THE discriminator, taken at the one instant it is decisive: readiness has just
            // claimed nothing arrived for a full 20s. If a plain non-blocking read succeeds here,
            // the data was in the socket the whole time and the readiness path lied. If it returns
            // WouldBlock, the socket really is empty and the datagrams never persisted -- which
            // sends the hunt upstream instead. Every earlier "can_recv() was false" observation
            // came from a log that only fires when it is false, and so could not tell these apart.
            let post = match sock.recv_from(&mut buf) {
                Ok((n, _)) => format!("READ_{}_BYTES_AFTER_IDLE", n),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => "empty".to_string(),
                Err(e) => format!("err:{:?}", e.kind()),
            };
            println!("UDPECHO post-idle-probe={}", post);
            // Print the *measured* wait next to the bound: an exit that states only its own
            // parameter can never disagree with itself, which is how this survived several
            // rounds of investigation while exiting after microseconds.
            println!(
                "UDPECHO exit-idle recv={} sent={} wouldblock={} waitwrite_timeout={} waited={:?} of {:?} multidrain={} maxdrain={}",
                recvs, sends, wb, wwfail, idle_start.elapsed(), idle, multidrain, maxdrain
            );
            // Expected exit: the benchmark finished and stopped sending.
            return Ok(());
        }
        t_wait_ret = mono_ns();
        // Drain every datagram the socket holds, not one per readiness event.
        //
        // Measured (smoltcp's own `net_trace!` on the peer): 109 datagrams were accepted into this
        // socket's receive buffer and the application dequeued 9 -- exactly one per rising edge of
        // the readiness word. Reading a single datagram per wake makes throughput a function of
        // how many edges the readiness path happens to produce rather than of how much data
        // arrived, and a socket that stays continuously readable produces no further edges at all.
        // Draining is what an echo server should do regardless; it also makes the loop correct
        // under a readiness path that under-reports, which this one demonstrably does.
        let mut drained = 0usize;
        loop {
            // see MULTIDRAIN below: the count itself is the readiness tripwire
            let (n, from) = match sock.recv_from(&mut buf) {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            };
            let t_recv = mono_ns();
            if drained == 0 {
                // wait-return -> first recv-return: the app-side remainder once the kevent wait
                // has reported readiness. With KQWAKE and UDPRISE this closes the split of the
                // rise -> recv gap into kernel-wake / kevent-bookkeeping / recv-call thirds.
                static WSUM: AtomicU64 = AtomicU64::new(0);
                static WCNT: AtomicU64 = AtomicU64::new(0);
                WSUM.fetch_add(t_recv.saturating_sub(t_wait_ret), Ordering::Relaxed);
                let n = WCNT.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    println!(
                        "WAITSPLIT n={} wait_ret_to_recv_avg_us={}",
                        n,
                        WSUM.load(Ordering::Relaxed) / n / 1000
                    );
                }
            }
            recvs += 1;
            drained += 1;
            // Latency-decomposition stamps for sysbench's net_udp_latency (the only user of
            // serve-udp-echo): a datagram carrying the magic gets our recv/send times at
            // [16..24]/[24..32], on the shared kernel clock epoch. Anything else is echoed
            // byte-identical.
            if n >= 32 && &buf[4..8] == b"TSTM" {
                buf[16..24].copy_from_slice(&t_recv.to_le_bytes());
                buf[24..32].copy_from_slice(&mono_ns().to_le_bytes());
            }
            echo_one(&sock, &buf[..n], from, &mut sends, &mut wb, &mut wwfail, fd);
        }
        if drained == 0 {
            // Readiness said ready and the socket had nothing: a spurious wake. Counted so it
            // cannot masquerade as progress.
            notready += 1;
        }
        // The drain fixes throughput and would otherwise HIDE the defect that made it necessary:
        // a consumer that reads everything available never notices that it was told about only
        // one. Every other consumer in this system already compensates the same way -- serve_echo
        // streams to EOF, TCP retransmits, the bench spins to drive its own engine -- which is
        // very likely why this has survived repeated investigation. So record it: draining more
        // than one datagram per readiness event means readiness reported one arrival and the
        // socket held several, which is the under-report itself, measured.
        if drained > 1 {
            multidrain += 1;
            if drained > maxdrain {
                maxdrain = drained;
            }
        }
    }
}

/// Echo one datagram back, tolerating transmit backpressure.
///
/// `WouldBlock` here is backpressure, not failure: twz-rt returns it instead of panicking on a full
/// tx buffer. One bounded wait for writability, then drop the datagram -- dropping is correct for
/// UDP and is what the client's sequence check survives; dying is not, and an echo server that
/// exits on its peer's backpressure wedges the whole boot.
fn echo_one(
    sock: &UdpSocket,
    payload: &[u8],
    from: std::net::SocketAddr,
    sends: &mut u64,
    wb: &mut u64,
    wwfail: &mut u64,
    fd: RawFd,
) {
    let mut waited = false;
    loop {
        match sock.send_to(payload, from) {
            Ok(_) => {
                *sends += 1;
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                *wb += 1;
                if waited || !wait_writable(fd, Duration::from_secs(1)) {
                    *wwfail += 1;
                    return;
                }
                waited = true;
            }
            Err(_) => return,
        }
    }
}
