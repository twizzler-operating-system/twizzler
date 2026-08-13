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
    io::{twz_rt_fd_pread, IoCtx, IoFlags},
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

// A watchdog thread was tried here and removed: it made things worse, for a reason worth keeping.
// `twz_rt_exit` calls `sys_thread_exit` for the *calling* thread only, so the compartment stays
// alive as long as any other thread does. A detached thread sleeping out a watchdog budget
// therefore holds the whole binary open for that long after `main` returns -- `unittest` waits on
// the compartment, and the run is lost to the very symptom the watchdog was meant to prevent.
// Bounding a hang has to come from not blocking indefinitely in the first place (see
// `accept_within`), not from a timer thread, until `exit` terminates a compartment outright.

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

/// Upper bound on how long a `connect-idle` peer holds its connection open.
///
/// A cap, not a duration: the peer holds until we drop our end and falls back on this only if that
/// never comes (`wait_for_close` in the peer). Every test below therefore drops its stream before
/// waiting on the peer, and pays a poll interval instead of this whole span.
///
/// Being a cap is also what lets it exceed `PEER_TIMEOUT`, which it must: the peer has to outlast
/// the parent's accept, and an accept is allowed to take `PEER_TIMEOUT`. At the old 4s a slow
/// enough boot could have the peer hang up *before* the accept it was waiting for -- costing 4s on
/// every passing run to buy a bound that was too short for the failing one.
const HOLD_MS: u64 = 20_000;

// --- listener readiness -------------------------------------------------------------------

/// A connection waiting for accept() must be reported readable even though the client has sent no
/// data. `SmolTcpListener::can_read` used to answer `can_recv()`, which is false for an
/// established-but-silent connection, so a poller sat blocked with a connection pending.
#[test]
fn listener_reports_pending_connection_without_data() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7701").expect("bind");
    let kq = register(listener.as_raw_fd(), EVFILT_READ, false);

    let peer = spawn_peer(
        "10.0.2.101",
        "connect-idle",
        "10.0.2.100:7701",
        &HOLD_MS.to_string(),
    );

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

    let mut peer = spawn_peer(
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
        if !wait_ready(kq, PEER_TIMEOUT) {
            // Two unrelated faults both show up as silence here, and the message has to say which:
            // if the level-triggered probe reports a connection pending *right now*, the EV_CLEAR
            // registration is stuck suppressed and this is LISTENER-REARM proper; if it does not,
            // nothing ever arrived and the fault is below the readiness layer (a lost SYN, a
            // handshake stalled in SYN-RECEIVED, a peer that never got the previous close).
            let pending = wait_ready(probe, Duration::ZERO);
            let peer_exited = peer.try_wait().ok().flatten();
            panic!(
                "EV_CLEAR listener went silent after {} of {} connections (LISTENER-REARM); \
                 level says pending: {}, peer: {:?}",
                accepted, REARM_CONNECTIONS, pending, peer_exited
            );
        }
        while accepted < REARM_CONNECTIONS && wait_ready(probe, Duration::ZERO) {
            let mut stream = accept_within(&listener, PEER_TIMEOUT, "rearm accept");
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf);
            // Explicit rather than relying on drop: the peer is blocked reading until we close,
            // and this test is about listener re-arm, not about what drop emits
            // (`dropping_a_stream_delivers_eof` covers that).
            let _ = stream.shutdown(std::net::Shutdown::Both);
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
    let peer = spawn_peer(
        "10.0.2.104",
        "connect-idle",
        "10.0.2.100:7704",
        &HOLD_MS.to_string(),
    );
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
    let peer = spawn_peer(
        "10.0.2.105",
        "connect-idle",
        "10.0.2.100:7705",
        &HOLD_MS.to_string(),
    );
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

// --- close ----------------------------------------------------------------------------------

/// How long the `connect-drop` peer stays alive after dropping its stream.
///
/// What it has to cover is the gap between the drop and the engine's next poll pass putting the FIN
/// on the wire, because nothing drains the engine at compartment exit. That pass is not waiting on
/// a timer: `TcpStreamInner::drop` calls `ENGINE.wake()` right after closing the socket, so the FIN
/// goes out within a poll iteration and this is three orders of magnitude of margin on top.
///
/// It is not required to outlast our read below: the two clocks start at different times (the peer
/// drops before our accept completes), and once the FIN is on the wire the peer's exit cannot
/// retract it. That asymmetry is why this can shrink while `EOF_TIMEOUT` stays generous -- the
/// budget for *detecting* the FIN is separate from the budget for emitting it.
const LINGER_MS: u64 = 500;
const EOF_TIMEOUT: Duration = Duration::from_secs(6);

/// Dropping a `TcpStream` must emit a FIN, so the other end sees EOF.
///
/// `TcpStreamInner::drop` used to only hand its socket to the engine's tracking list, and
/// `State::Closed` is the one state `check_tracking` releases on. A stream dropped without an
/// explicit `shutdown()`, whose peer never reset it, could not get there -- so it stayed half-open
/// for the life of the process: the peer never saw EOF, the socket was never released from the
/// socket set, and its ephemeral port was never returned.
#[test]
fn dropping_a_stream_delivers_eof() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7710").expect("bind");
    let peer = spawn_peer(
        "10.0.2.110",
        "connect-drop",
        "10.0.2.100:7710",
        &LINGER_MS.to_string(),
    );

    let stream = accept_within(&listener, PEER_TIMEOUT, "connect-drop");
    let fd = stream.as_raw_fd();

    // Non-blocking reads against a deadline, rather than a blocking read that returns 0 at EOF.
    // Two reasons, both of which rule out the more obvious spellings: EOF is not visible through
    // the readiness path at all (a stream's read word is `can_recv()`, which is false once the
    // buffer is drained whether or not a FIN arrived), so `wait_ready` cannot bound the wait; and a
    // blocking read that never sees a FIN -- exactly the regression under test -- would hang the
    // whole suite, since there is no per-test timeout and a watchdog thread cannot be used (see
    // the note above).
    let deadline = Instant::now() + EOF_TIMEOUT;
    let mut payload = Vec::new();
    let mut last_err = None;
    let mut eof = false;
    while Instant::now() < deadline {
        let mut buf = [0u8; 64];
        let mut ctx = IoCtx::new(None, IoFlags::NONBLOCKING, None);
        match twz_rt_fd_pread(fd, &mut buf, &mut ctx) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(n) => payload.extend_from_slice(&buf[..n]),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    assert!(
        eof,
        "no FIN within {:?} after the peer dropped its stream (read {:?}, last error {:?})",
        EOF_TIMEOUT, payload, last_err
    );
    assert_eq!(payload, b"bye", "peer's data did not arrive intact");

    drop(stream);
    expect_peer_ok(peer, "connect-drop");
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
    let holder = spawn_peer(
        "10.0.2.108",
        "connect-idle",
        "10.0.2.100:7708",
        &HOLD_MS.to_string(),
    );
    let held = accept_within(&holder_listener, PEER_TIMEOUT, "third-compartment holder");

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
    // Explicit, and it has to come before the wait: the holder holds until we close, so leaving
    // this to end-of-scope would leave it waiting out `HOLD_MS` with us waiting on it. The echo
    // above is what this test is about, and it has already happened with the holder live.
    drop(held);
    expect_peer_ok(holder, "connect-idle holder");
}

fn main() {
    println!("net_test: run with --test");
}

// --- coverage the shapes above do not reach --------------------------------------------------

/// The byte a bulk transfer carries at offset `i`. Must match `net_test_peer`'s copy.
fn bulk_byte(i: usize) -> u8 {
    (i % 251) as u8
}

/// Size of the bulk transfer. Chosen to exceed a single segment and a default window by enough to
/// force segmentation, window updates and multiple poll passes -- everything else in this file
/// moves eight bytes, which all fit in one segment and never exercise any of that.
const BULK_LEN: usize = 64 * 1024;

/// Closing a just-accepted connection must deliver a FIN.
///
/// The regression test for the accept-in-SYN-RECEIVED bug. `accept()` used to hand back a socket
/// whose handshake had not finished (`is_active()` is true in SYN-RECEIVED), and closing one in
/// that state left smoltcp reading the handshake's ACK as an ACK of a FIN it had never sent: the
/// socket went to FIN-WAIT-2, nothing reached the wire, and the peer hung until its cap.
///
/// Distinct from `dropping_a_stream_delivers_eof`, which closes the *connecting* side after it has
/// sent data. This one closes the *accepted* side with no I/O at all, which is the only shape that
/// reaches the bug -- the neighbours all hold their stream long enough for the handshake to land.
#[test]
fn closing_a_just_accepted_stream_delivers_eof() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7711").expect("bind");
    let peer = spawn_peer(
        "10.0.2.111",
        "connect-idle",
        "10.0.2.100:7711",
        &HOLD_MS.to_string(),
    );

    let stream = accept_within(&listener, PEER_TIMEOUT, "just-accepted close");
    // No read, no write, no shutdown: straight to close, while the handshake is at its youngest.
    drop(stream);

    // connect-idle exits nonzero if it hit its cap without seeing EOF, so this is the assertion --
    // and it fails outright rather than merely running slowly, which is how the bug used to read.
    expect_peer_ok(peer, "connect-idle after immediate close");
}

/// A transfer larger than one segment must arrive intact and in order.
#[test]
fn bulk_transfer_arrives_intact() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7713").expect("bind");
    let peer = spawn_peer(
        "10.0.2.113",
        "connect-recv",
        "10.0.2.100:7713",
        &BULK_LEN.to_string(),
    );

    let mut stream = accept_within(&listener, PEER_TIMEOUT, "bulk transfer");
    let data: Vec<u8> = (0..BULK_LEN).map(bulk_byte).collect();
    stream.write_all(&data).expect("write bulk");
    stream.flush().expect("flush");
    // The close is part of the test: the peer requires EOF at exactly BULK_LEN, so the queued data
    // has to drain ahead of the FIN rather than being cut off by it.
    drop(stream);

    expect_peer_ok(peer, "connect-recv");
}

/// Shutting down the write half must deliver EOF without closing the other direction.
///
/// The request/response shape: the peer says "that is the whole request" with `shutdown(Write)` and
/// still expects its answer back. A close that tore down both directions would pass every other
/// test here and break this one.
#[test]
fn half_close_still_allows_a_reply() {
    setup();
    let listener = TcpListener::bind("0.0.0.0:7714").expect("bind");
    let peer = spawn_peer(
        "10.0.2.114",
        "connect-halfclose",
        "10.0.2.100:7714",
        "request",
    );

    let stream = accept_within(&listener, PEER_TIMEOUT, "half close");
    let fd = stream.as_raw_fd();

    // Read to EOF the bounded way, for the reason `dropping_a_stream_delivers_eof` spells out: EOF
    // is not visible through the readiness path, and a blocking read that never sees the peer's
    // half-close -- the regression this guards -- would hang the suite.
    let deadline = Instant::now() + EOF_TIMEOUT;
    let mut request = Vec::new();
    let mut eof = false;
    while Instant::now() < deadline {
        let mut buf = [0u8; 64];
        let mut ctx = IoCtx::new(None, IoFlags::NONBLOCKING, None);
        match twz_rt_fd_pread(fd, &mut buf, &mut ctx) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(n) => request.extend_from_slice(&buf[..n]),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert!(
        eof,
        "shutdown(Write) on the peer never delivered EOF (read {:?})",
        request
    );
    assert_eq!(request, b"request", "half-closed request arrived wrong");

    // The half we were never told about must still work.
    let mut stream = stream;
    stream
        .write_all(b"request")
        .expect("reply after half close");
    stream.flush().expect("flush");
    drop(stream);

    expect_peer_ok(peer, "connect-halfclose");
}

/// How many pings `udp_round_trip_replies_to_sender` lets its peer try. Same loss reasoning as
/// `UDP_SEND_COUNT`, but now either direction can drop one, so the peer retries until answered.
const UDP_ECHO_ATTEMPTS: usize = 20;

/// A datagram must round-trip, and `recv_from` must name the sender well enough to answer it.
///
/// The existing UDP test only ever receives, and discards the address it receives from; nothing
/// covered sending a datagram or the address being right. Replying to whatever `recv_from` reported
/// tests both at once: a wrong source address means the pong goes nowhere and the peer fails.
#[test]
fn udp_round_trip_replies_to_sender() {
    setup();
    // Our own address, not the wildcard: a 0.0.0.0 UDP bind currently receives nothing. See the
    // note in `udp_stops_reporting_readable_once_drained`.
    let sock = UdpSocket::bind("10.0.2.100:7712").expect("bind");
    let kq = register(sock.as_raw_fd(), EVFILT_READ, false);
    let mut peer = spawn_peer(
        "10.0.2.112",
        "udp-echo",
        "10.0.2.100:7712",
        &UDP_ECHO_ATTEMPTS.to_string(),
    );

    // Keep answering until the peer is satisfied and exits, rather than replying once and waiting:
    // if that one pong is lost the peer pings again, and nobody would be listening.
    let deadline = Instant::now() + PEER_TIMEOUT;
    let mut answered = 0;
    let status = loop {
        if let Some(status) = peer.try_wait().expect("try_wait udp-echo") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "udp-echo peer never finished ({} pings answered)",
            answered
        );
        if !wait_ready(kq, Duration::from_millis(200)) {
            continue;
        }
        let mut buf = [0u8; 64];
        let (n, from) = sock.recv_from(&mut buf).expect("recv");
        assert!(buf[..n].starts_with(b"ping"), "unexpected datagram");
        assert_eq!(
            from.ip(),
            "10.0.2.112".parse::<std::net::IpAddr>().unwrap(),
            "recv_from reported the wrong sender"
        );
        sock.send_to(b"pong", from).expect("reply");
        answered += 1;
    };

    assert!(answered > 0, "peer exited before any ping arrived");
    assert!(status.success(), "udp-echo peer failed");
}
