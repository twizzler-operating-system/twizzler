//! Networking tests that need a real peer in a *different compartment*.
//!
//! Everything here exists because a single process cannot test the connect-meets-accept path: one
//! compartment means one `twz-rt` socket engine, one smoltcp interface and one address, so a
//! client and a server in the same binary would never exchange a packet. Each test that needs a
//! peer spawns `net_test_peer` (see that crate) and grades it by exit status.
//!
//! Readiness is checked through `twz_rt_fd_kevent` directly rather than libc `poll`, for the same
//! reason `kqueue_test` does: it is the path mio and therefore tokio actually take, and it keeps
//! this crate free of a libc dependency.
//!
//! Addressing. net-srv hands out addresses in whatever order compartments happen to open a client,
//! which leaves neither side able to name the other ahead of time. Both ends therefore pin their
//! address with `TWZ_NET_ADDR` (honoured by the engine in twz-rt). Tests run in parallel, so each
//! one owns a distinct peer address *and* a distinct port -- net-srv's port assigner is global
//! across compartments, and sshd already holds 5555.
//!
//! These tests are also, collectively, the regression test for multiple networked compartments
//! coexisting at all: every one of them has at least three live net-srv clients (sshd, this
//! binary, the peer). Before per-client MACs, the second client's smoltcp answered every frame it
//! had no socket for with an RST, tearing down the first client's connections.

use std::{
    io::{Read, Write},
    net::{TcpListener, UdpSocket},
    os::fd::AsRawFd,
    process::{Child, Command},
    sync::Once,
    time::{Duration, Instant},
};

use twizzler_rt_abi::{
    bindings::{
        kevent, option_duration, twz_rt_fd_kevent, EVFILT_READ, EVFILT_WRITE, EV_ADD, EV_CLEAR,
        EV_ERROR, EV_RECEIPT,
    },
    fd::RawFd,
};

/// This binary's own address. One engine per process, so one address for every test here.
const SELF_ADDR: &str = "10.0.2.100";

static SETUP: Once = Once::new();

/// Pin our address before anything touches a socket. The engine is initialised lazily on first
/// socket use, so this must happen first -- `Once` is what orders it against tests running in
/// parallel, every one of which calls this before doing anything else.
fn setup() {
    SETUP.call_once(|| {
        std::env::set_var("TWZ_NET_ADDR", SELF_ADDR);
    });
}

/// Directories a spawned peer may live in, mirroring `unittest`'s search order.
fn peer_path() -> String {
    ["/pkg/twizzler/bin", "/initrd"]
        .iter()
        .map(|dir| format!("{}/net_test_peer", dir))
        .find(|path| std::fs::metadata(path).is_ok())
        .unwrap_or_else(|| "/pkg/twizzler/bin/net_test_peer".to_string())
}

/// Spawn the peer at `peer_addr`, talking to `target`.
fn spawn_peer(peer_addr: &str, mode: &str, target: &str, arg: &str) -> Child {
    Command::new(peer_path())
        .args([mode, target, arg])
        // Always explicit: our own TWZ_NET_ADDR would otherwise be inherited, and two stacks
        // sharing an address makes ARP pick a winner at random.
        .env("TWZ_NET_ADDR", peer_addr)
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {}", peer_path(), e))
}

fn peer_status(mut child: Child, what: &str) -> bool {
    child
        .wait()
        .unwrap_or_else(|e| panic!("wait for peer ({}): {}", what, e))
        .success()
}

fn expect_peer_ok(child: Child, what: &str) {
    assert!(peer_status(child, what), "peer ({}) failed", what);
}

fn kevent_call(
    kq: RawFd,
    changelist: &[kevent],
    eventlist: &mut [kevent],
    timeout: Option<Duration>,
) -> usize {
    let timeout: option_duration = timeout.into();
    let res = unsafe {
        twz_rt_fd_kevent(
            kq,
            changelist.as_ptr(),
            changelist.len(),
            eventlist.as_mut_ptr(),
            eventlist.len(),
            timeout,
        )
    };
    let r: twizzler_rt_abi::Result<usize> = res.into();
    r.expect("kevent")
}

fn change(ident: RawFd, filter: i16, flags: u16) -> kevent {
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

/// Register `fd` for `filter` and return the kqueue. `EV_CLEAR` is the edge-triggered mode mio
/// uses; without it the registration is level-triggered.
///
/// `EV_RECEIPT` matters here and is not decoration: without it the registering call goes on to
/// wait, and an already-ready source (a connected socket is writable immediately) would have its
/// readiness reported and consumed by *this* call rather than the wait the test is about. Sizing
/// the eventlist to the changelist is what makes the receipt fill it and force an early return --
/// mio's `Selector::register` has exactly this shape.
fn register(fd: RawFd, filter: i16, clear: bool) -> RawFd {
    let kq = twizzler_rt_abi::fd::twz_rt_fd_open_kqueue(0).expect("kqueue");
    let mut flags = EV_ADD | EV_RECEIPT;
    if clear {
        flags |= EV_CLEAR;
    }
    let mut out = [change(0, 0, 0); 1];
    let n = kevent_call(
        kq,
        &[change(fd, filter, flags)],
        &mut out,
        Some(Duration::ZERO),
    );
    assert_eq!(n, 1, "expected a receipt for the registration");
    assert_eq!(out[0].flags & EV_ERROR, EV_ERROR, "receipt is an EV_ERROR");
    assert_eq!(out[0].data, 0, "registration failed: errno {}", out[0].data);
    kq
}

/// Wait for one readiness report, returning false on timeout.
fn wait_ready(kq: RawFd, timeout: Duration) -> bool {
    let mut out = [change(0, 0, 0); 4];
    kevent_call(kq, &[], &mut out, Some(timeout)) > 0
}

/// `accept()`, but fail rather than hang if nothing arrives.
///
/// A bare `accept()` blocks forever, and neither the Rust harness nor `unittest` imposes a
/// per-test timeout -- so a peer that never connects would wedge the entire suite instead of
/// failing one case. Waiting for readiness first (level-triggered, so an already-pending
/// connection still reports) means the `accept()` that follows cannot block.
fn accept_within(listener: &TcpListener, timeout: Duration, what: &str) -> std::net::TcpStream {
    let kq = register(listener.as_raw_fd(), EVFILT_READ, false);
    let ready = wait_ready(kq, timeout);
    twizzler_rt_abi::fd::twz_rt_fd_close(kq);
    assert!(
        ready,
        "no connection arrived within {:?} ({})",
        timeout, what
    );
    listener.accept().expect("accept").0
}

/// How long any single test will wait for its peer before giving up.
///
/// Generous on purpose. Eight tests run in parallel, each spawning a compartment, and on a
/// single-CPU boot that is nine compartments' worth of loading contending for one core. The
/// budget is "long enough that a pass is never in doubt", not a latency assertion -- the tests
/// that do assert on timing assert that a wait *lasted*, which slowness cannot break.
const PEER_TIMEOUT: Duration = Duration::from_secs(15);

/// Datagrams the UDP peer sends, spread over `UDP_SEND_COUNT * 150ms`.
///
/// Far more than the one the test needs, because UDP has no retransmit and the datagram has two
/// lossy points to survive: smoltcp drops one outright when the destination's link-layer address
/// is still unresolved, and net-srv's on-host delivery drops one when the receiving compartment
/// has no free rx packet -- which a loaded single-CPU boot can produce. Four was not enough:
/// `release-kvm-smp1` lost all four while every other configuration passed.
const UDP_SEND_COUNT: usize = 20;

// --- listener readiness -------------------------------------------------------------------

/// A connection waiting for accept() must be reported readable even though the client has sent no
/// data. `SmolTcpListener::can_read` used to answer `can_recv()`, which is false for an
/// established-but-silent connection, so a poller sat blocked with a connection pending.
#[test]
fn listener_reports_pending_connection_without_data() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7701").expect("bind");
    let kq = register(listener.as_raw_fd(), EVFILT_READ, false);

    let peer = spawn_peer("10.0.2.101", "connect-idle", "10.0.2.100:7701", "2000");

    assert!(
        wait_ready(kq, PEER_TIMEOUT),
        "listener never reported readable for a pending connection"
    );
    let stream = accept_within(&listener, PEER_TIMEOUT, "pending connection");
    drop(stream);
    expect_peer_ok(peer, "connect-idle");
}

/// How many connections `clear_listener_rearms_across_accepts` drives through the listener.
///
/// Two would be enough to catch the original bug -- with it, connection 1 reports and connection 2
/// is silent. This is larger for a reason the bug does not motivate but the fix does: smoltcp
/// hands each SYN to the first matching socket in `SocketSet` order, so with `BACKLOG = 8` the
/// first eight connections all land on sockets `bind()` created. Only from the ninth does a
/// connection reach a socket that `accept()` added to the group, which is the wiring the fix
/// introduced and the part most likely to be wrong.
const REARM_CONNECTIONS: usize = 10;

/// LISTENER-REARM. An EV_CLEAR registration on a listener must re-arm after each accept.
///
/// `accept()` swaps a fresh socket into the backlog slot, so a suppression token taken before an
/// accept used to be compared against a different socket's falling-edge counter -- `0 != 0` reads
/// as "no fall since", and the registration stayed silent forever.
///
/// The peer leaves a gap between connections precisely so each accept lands in between: were two
/// pending at once the listener's readiness would never fall, and a correct edge-triggered
/// implementation would rightly stay silent -- the test would be asserting the wrong thing.
#[test]
fn clear_listener_rearms_across_accepts() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7702").expect("bind");
    let kq = register(listener.as_raw_fd(), EVFILT_READ, true);

    let peer = spawn_peer(
        "10.0.2.102",
        "connect-n",
        "10.0.2.100:7702",
        &REARM_CONNECTIONS.to_string(),
    );

    // A level-triggered registration kept alongside the edge one, used with a zero timeout purely
    // as a "is another connection pending right now?" probe. An edge-triggered consumer must drain
    // until empty -- accepting exactly one per report would leave a second connection sitting in
    // the backlog with the level still high, no falling edge, and a correctly-suppressed
    // registration that then looks like the bug.
    let probe = register(listener.as_raw_fd(), EVFILT_READ, false);

    let mut accepted = 0;
    while accepted < REARM_CONNECTIONS {
        assert!(
            wait_ready(kq, PEER_TIMEOUT),
            "EV_CLEAR listener went silent after {} of {} connections (LISTENER-REARM)",
            accepted,
            REARM_CONNECTIONS
        );
        while accepted < REARM_CONNECTIONS && wait_ready(probe, Duration::ZERO) {
            let mut stream = accept_within(&listener, PEER_TIMEOUT, "rearm accept");
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf);
            drop(stream);
            accepted += 1;
        }
    }

    expect_peer_ok(peer, "connect-n");
}

/// The basic path in both directions: accept, read what the peer sent, echo it back.
#[test]
fn accept_and_echo() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7703").expect("bind");
    let peer = spawn_peer("10.0.2.103", "connect-echo", "10.0.2.100:7703", "twizzler");

    let mut stream = accept_within(&listener, PEER_TIMEOUT, "accept_and_echo");
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).expect("read");
    assert_eq!(&buf, b"twizzler");
    stream.write_all(&buf).expect("write");
    stream.flush().expect("flush");

    expect_peer_ok(peer, "connect-echo");
}

// --- idle sockets must not spin ----------------------------------------------------------

/// A connected socket is essentially always writable, so a level-triggered EVFILT_WRITE returns
/// instantly forever -- that is what made tokio's reactor spin a core with no forward progress.
/// Under EV_CLEAR the readiness is reported once and then must go quiet.
#[test]
fn clear_stream_reports_writable_once() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7704").expect("bind");
    let peer = spawn_peer("10.0.2.104", "connect-idle", "10.0.2.100:7704", "3000");
    let stream = accept_within(&listener, PEER_TIMEOUT, "writable-once");

    let kq = register(stream.as_raw_fd(), EVFILT_WRITE, true);
    assert!(
        wait_ready(kq, PEER_TIMEOUT),
        "connected socket never reported writable"
    );
    assert!(
        !wait_ready(kq, Duration::from_millis(500)),
        "EV_CLEAR write registration re-reported an unchanged writability"
    );

    drop(stream);
    expect_peer_ok(peer, "connect-idle");
}

/// An idle *readable* registration must block for the whole timeout rather than returning early
/// claiming nothing is ready -- the symptom of `mark_waiter` waking the wrong side.
#[test]
fn idle_stream_read_waits_out_its_timeout() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7705").expect("bind");
    let peer = spawn_peer("10.0.2.105", "connect-idle", "10.0.2.100:7705", "3000");
    let stream = accept_within(&listener, PEER_TIMEOUT, "idle-read");

    let kq = register(stream.as_raw_fd(), EVFILT_READ, false);
    let start = Instant::now();
    let ready = wait_ready(kq, Duration::from_millis(800));
    let waited = start.elapsed();
    assert!(!ready, "idle socket reported readable with no data sent");
    assert!(
        waited >= Duration::from_millis(700),
        "kevent returned after {:?}, well short of its timeout",
        waited
    );

    drop(stream);
    expect_peer_ok(peer, "connect-idle");
}

// --- UDP ----------------------------------------------------------------------------------

/// Part 1 bug 1: UDP wait words were only ever set, never cleared, so once a socket had received
/// anything it claimed to be readable forever and async readers spun. After draining the one
/// datagram, a readiness wait must actually block.
#[test]
fn udp_stops_reporting_readable_once_drained() {
    setup();
    // Bound to our specific address, not the wildcard: `UdpSocket::bind` hands smoltcp
    // `(addr, port)`, whose `From` impl yields `addr: Some(0.0.0.0)` rather than `None`, and
    // `udp::Socket::accepts` then rejects every datagram whose destination is not literally
    // 0.0.0.0. Wildcard UDP binds receive nothing -- a real bug, but a separate one, and this
    // test is about readiness rather than binding.
    let sock = UdpSocket::bind("10.0.2.100:7706").expect("bind");
    let kq = register(sock.as_raw_fd(), EVFILT_READ, false);
    let peer = spawn_peer(
        "10.0.2.106",
        "udp-send",
        "10.0.2.100:7706",
        &UDP_SEND_COUNT.to_string(),
    );

    assert!(
        wait_ready(kq, PEER_TIMEOUT),
        "UDP socket never reported an incoming datagram"
    );
    // Let the peer finish sending before draining, so "drained" is a stable state rather than a
    // race against datagrams still in flight.
    expect_peer_ok(peer, "udp-send");

    let mut received = 0;
    let mut buf = [0u8; 64];
    while wait_ready(kq, Duration::ZERO) {
        let (n, _from) = sock.recv_from(&mut buf).expect("recv");
        assert!(buf[..n].starts_with(b"ping"), "unexpected datagram");
        received += 1;
    }
    assert!(received > 0, "readiness reported but nothing to receive");

    let start = Instant::now();
    let ready = wait_ready(kq, Duration::from_millis(800));
    assert!(!ready, "drained UDP socket still reported readable");
    assert!(
        start.elapsed() >= Duration::from_millis(700),
        "UDP readiness wait returned early"
    );
}

// --- multiple compartments ----------------------------------------------------------------

/// Connecting to a port nobody is listening on must be refused rather than hang.
///
/// The bound listener is not incidental: it forces this compartment's engine up so that
/// `10.0.2.100` is a live address with a stack behind it. Without it the peer's SYN would go
/// unanswered and the test would measure a timeout instead of a refusal.
#[test]
fn connect_to_closed_port_is_refused() {
    setup();
    let _up = TcpListener::bind("0.0.0.0:7707").expect("bind");
    let peer = spawn_peer("10.0.2.107", "connect-idle", "10.0.2.100:7799", "100");
    assert!(
        !peer_status(peer, "connect to closed port"),
        "connecting to a closed port reported success"
    );
}

/// A connection between two compartments must survive a *third* compartment holding a live net
/// client. This is the shape that used to break outright: before per-client MACs every client saw
/// every frame, and the idle one's smoltcp answered each segment it had no socket for with an RST.
#[test]
fn stream_survives_a_third_networked_compartment() {
    setup();
    let holder_listener = TcpListener::bind("0.0.0.0:7708").expect("bind holder listener");
    let holder = spawn_peer("10.0.2.108", "connect-idle", "10.0.2.100:7708", "4000");
    let _held = accept_within(&holder_listener, PEER_TIMEOUT, "third-compartment holder");

    // With the holder's stack live and idle, run a full exchange with a different peer.
    let listener = TcpListener::bind("0.0.0.0:7709").expect("bind");
    let peer = spawn_peer("10.0.2.109", "connect-echo", "10.0.2.100:7709", "coexist");

    let mut stream = accept_within(&listener, PEER_TIMEOUT, "coexist echo");
    let mut buf = [0u8; 7];
    stream.read_exact(&mut buf).expect("read");
    assert_eq!(&buf, b"coexist");
    stream.write_all(&buf).expect("write");
    stream.flush().expect("flush");

    expect_peer_ok(peer, "connect-echo alongside a third compartment");
    expect_peer_ok(holder, "connect-idle holder");
}

fn main() {
    println!("net_test: run with --test");
}
