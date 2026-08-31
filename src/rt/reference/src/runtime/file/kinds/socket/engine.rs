use std::{
    collections::{HashMap, HashSet},
    io::ErrorKind,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
    usize,
};

use secgate::{
    util::{Descriptor, Handle},
    TwzError,
};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    socket::{
        dns::Socket as DnsSocket,
        // NOTE: this shadows the `Socket` *enum*. A bare `Socket` in this file means
        // `tcp::Socket`, so anything that wants the enum -- `SocketSet::get`/`get_mut`
        // turbofishes, `socket_readiness`'s parameter -- must spell out
        // `smoltcp::socket::Socket`.
        tcp::{Socket, State},
        udp::Socket as SmolUdpSocket,
    },
    time::{Duration, Instant},
    wire::{IpAddress, IpCidr, Ipv4Address, Ipv6Address},
};
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep,
    ThreadSyncWake,
};
use twizzler_net::{net_alloc_port, net_release_port, NetClient, NetClientConfig};
use twizzler_rt_abi::bindings::{wait_kind, WAIT_READ, WAIT_WRITE};

pub struct Engine {
    pub(super) core: Arc<Mutex<Core>>,
    waiter: Arc<Condvar>,
    notify: Arc<AtomicU64>,
    _polling_thread: JoinHandle<()>,
    nc_handle: Descriptor,
}

#[derive(Clone, Copy)]
pub(super) enum SockKind {
    Tcp,
    Udp,
}

/// Identity of a readiness source. Usually one socket -- but a listener's accept queue is several
/// listening sockets whose readiness is the OR over the group, and `accept()` swaps handles in and
/// out of that group, so no individual `SocketHandle` can name the thing a poller registered
/// against.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum WaitKey {
    Sock(SocketHandle),
    Group(u64),
}

/// A listening socket is readable exactly when `accept()` will return a connection for it: an
/// established one. Not `is_active()`, which smoltcp makes true from SynReceived onward -- see the
/// handshake note at the bottom.
///
/// Deliberately a state predicate rather than an event, so the readiness cannot be lost between the
/// poll pass that observes it and the `accept()` that consumes it. `SmolTcpListener::can_read` must
/// agree with this: a live check that disagrees with the published word is what makes an
/// edge-triggered consumer wait for an edge that already happened.
///
/// It must *not* also cover "this socket has broken and needs rebinding" (`!is_open()`,
/// `!is_listening()`), which is what the old retire-on-first-connection branch in `poll` tested
/// for. Those are maintenance, not readiness: advertising them makes a poller report readable while
/// `accept()` finds nothing active, takes its repair branch, and blocks -- an outright hang, which
/// is how this was found. The repair still happens, because a blocked `accept()` re-runs on every
/// `Core::poll` condvar notify rather than waiting on these words.
///
/// Excluding SYN-RECEIVED is the handshake note. `is_active()` is already true there, which offers
/// up a connection whose handshake has not finished; closing such a socket is unrecoverable, and
/// still is as of smoltcp 0.13.1 (re-checked on upgrade, line numbers from its `socket/tcp.rs`):
/// `close()` moves SYN-RECEIVED straight to FIN-WAIT-1 (1077), and `(sent_syn, sent_fin)` is
/// derived from the *current state* rather than from history (1566), so FIN-WAIT-1 reports "sent a
/// FIN, no outstanding SYN". The still-unacknowledged SYN is now invisible, `tx_buffer_start_seq`
/// is one too low (1780), and when the handshake's ACK finally arrives it satisfies
/// `tx_buffer.len() + 1 == ack_len` and is read as an ACK *of a FIN that was never transmitted*
/// (1787). The socket settles in FIN-WAIT-2 (1941), no FIN ever reaches the wire, and the peer
/// waits for an EOF that cannot come.
///
/// Waiting for the handshake closes that window and is what accept() is supposed to mean anyway.
/// The connection stays pending in the backlog meanwhile, and the transition to ESTABLISHED happens
/// inside a poll pass, which republishes readiness -- so a waiter still gets its edge.
///
/// The cost, which is real and currently unmitigated: a handshake that never completes (the client
/// vanished, or its final ACK is lost for good) pins a backlog slot invisibly and forever. It is
/// not ready, so it is never accepted; `accept()`'s repair branch only fires on `!is_open()`, and
/// SYN-RECEIVED is open, so it is never rebound; and smoltcp only reaps such a socket via
/// `timed_out()`, which needs `Socket::set_timeout`, defaults to `None`, and is never set here.
/// With `BACKLOG` of them a listener goes silently deaf. The fix would be a timeout on the
/// *listening* sockets, cleared in `accept()` -- note it must be cleared, because `remote_last_ts`
/// is refreshed on every received packet, so a timeout left on an accepted socket would abort
/// idle-but-healthy connections.
pub(super) fn listener_socket_ready(socket: &Socket<'_>) -> bool {
    socket.is_active() && socket.state() != State::SynReceived
}

/// Read-readiness of a connected stream: bytes buffered, or a close that makes the next read
/// return `Ok(0)` rather than block. Dropping the second term -- as bare `can_recv()` does --
/// leaves a poller asleep on a half-closed connection, waiting for data that by definition will
/// never arrive.
///
/// Call this from `read`'s branch condition, from `can_read`, and from `socket_readiness`, exactly
/// as `listener_socket_ready` is called from the listener's three. A readiness check that disagrees
/// with the read it predicts is the bug; one function is what stops the two drifting apart again.
/// In `read`'s EOF arm `can_recv()` is already false, so the call reduces to the close term.
///
/// `rx_shutdown` is per-fd state rather than socket state, so it cannot be read from a `&Socket`
/// and has to be passed. `socket_readiness` has no fd to read it from and passes `false`; see the
/// note there.
///
/// The pre-connection states are excluded because `may_recv()` is false there too, and a socket
/// still completing its handshake is not at EOF. No current path exposes an fd in `SynSent` --
/// nonblocking `connect` returns `InProgress` before constructing one -- so this is presently
/// belt-and-braces, and worth keeping for the nonblocking-connect implementation that would.
pub(super) fn stream_socket_ready(socket: &Socket<'_>, rx_shutdown: bool) -> bool {
    socket.can_recv()
        || ((!socket.may_recv() || rx_shutdown)
            && socket.state() != State::SynReceived
            && socket.state() != State::SynSent)
}

/// Read-readiness of a datagram socket, on the same contract as `stream_socket_ready`.
pub(super) fn udp_socket_ready(socket: &SmolUdpSocket<'_>, rx_shutdown: bool) -> bool {
    socket.can_recv() || !socket.is_open() || rx_shutdown
}

/// Number of entries in `Core::tracking`, readable without taking the core lock.
///
/// The poll thread ran `while check_tracking() {}` at the top of every cycle: it took the core
/// mutex and walked the list even when nothing had closed, and because it returned after a single
/// removal it re-took the lock and restarted the walk once per closed socket.
///
/// Rate-limiting the call would have been the wrong fix. This is the path that releases closed
/// sockets and returns their ports, so delaying it holds both for longer under connection churn --
/// the opposite of the direction this file has been moving all week. Making it free when there is
/// nothing to do costs one relaxed load, and makes the busy case one pass instead of N.
static TRACKING_LEN: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Core {
    socketset: SocketSet<'static>,
    ifaceset: Vec<IfaceSet>,
    tracking: Vec<(SocketHandle, u16, SockKind)>,
    /// Reused readiness-aggregation scratch for `poll` — it ran on every productive pass with a
    /// fresh alloc. Taken/returned around the borrow of `socketset`.
    agg_scratch: HashMap<WaitKey, (bool, bool)>,
    // Listening sockets -> the listener group they belong to. Membership is also what marks a
    // socket as a listener, which is what selects listener_socket_ready over can_recv/can_send.
    groups: HashMap<SocketHandle, u64>,
    /// Handles currently present in `socketset`.
    ///
    /// `SocketSet::get` panics on a handle whose slot has been removed, and `refresh_waiter` is
    /// reachable from `waitpoint`/`down_waitpoint` without the caller having established that the
    /// socket is still live -- a poll on a just-closed fd does exactly that. The scan this
    /// replaces was safe by accident: it walked the set and simply found nothing. This keeps that
    /// "not present -> not ready" answer while making the common case a hash lookup instead of a
    /// walk of every socket.
    live: HashSet<SocketHandle>,
}

struct IfaceSet {
    ifaces: Vec<Interface>,
    device: NetClient,
}

impl IfaceSet {
    fn new(device: NetClient) -> Self {
        let ifaces = Vec::new();
        Self { ifaces, device }
    }

    fn insert_iface(&mut self, iface: Interface) {
        self.ifaces.push(iface);
    }

    fn poll(&mut self, socketset: &mut SocketSet<'static>) -> bool {
        let mut ready = false;
        for iface in &mut self.ifaces {
            POLL_SUB.store(11, Ordering::Relaxed);
            // 0.13 returns PollResult rather than a bool; SocketStateChanged is the old `true`.
            ready |= iface.poll(Instant::now(), &mut self.device, socketset)
                == smoltcp::iface::PollResult::SocketStateChanged;
            POLL_SUB.store(12, Ordering::Relaxed);
        }
        // `iface.poll` is the only caller of `NetClient::transmit`, so this is the one place a
        // poll's egress can be flushed. Anything queued and not flushed here waits for the next
        // poll.
        POLL_SUB.store(13, Ordering::Relaxed);
        self.device.flush_tx();
        POLL_SUB.store(14, Ordering::Relaxed);
        // Reclaiming a tx packet unblocks a sender that had none, and smoltcp does not count it as
        // a socket state change -- so report it as readiness here rather than notifying on every
        // poll regardless. See `Pair::progress`.
        ready |= self.device.took_progress();
        ready
    }

    fn poll_time(&mut self, socketset: &mut SocketSet<'static>) -> Option<Duration> {
        let mut min_delay = None;
        for iface in &mut self.ifaces {
            if let Some(delay) = iface.poll_delay(Instant::now(), socketset) {
                min_delay = Some(min_delay.map_or(delay, |min: Duration| min.min(delay)));
            }
        }
        min_delay
    }

    fn find_iface_for(&mut self, _addr: SocketAddr) -> Option<&mut Interface> {
        // TODO
        self.ifaces.get_mut(0)
    }
    fn find_iface_for_dns(&mut self) -> Option<&mut Interface> {
        // TODO
        self.ifaces.get_mut(0)
    }
}

/// Diagnostic counters for the "a successful send never wakes the poll thread" hypothesis.
///
/// `blocking`'s fast path returns before `core.poll()` and before `wake()`, and smoltcp's
/// `poll_delay` is `None` for a compartment whose only socket has an empty tx buffer
/// (`udp::Socket::poll_at` -> `PollAt::Ingress` -> `None`). So a datagram queued by a successful
/// `send_slice` waits for an unrelated wakeup. If that is what happens, POLLS stays flat while
/// FAST_OK climbs.
///
/// Relaxed adds only: no wake, no lock, no branch on socket state. A wake here is the candidate
/// *fix*, and adding one would destroy the measurement meant to justify it.
static ENGINE_POLLS: AtomicU64 = AtomicU64::new(0);
/// Times the poll loop hit its immediate-repoll bound, i.e. smoltcp asked to be re-polled in
/// <100us at least `MAX_IMMEDIATE_REPOLLS` times in a row without the loop ever sleeping.
///
/// **This is the falsifier for the spin hypothesis.** If it stays 0 across boots then the loop
/// never spins, the bound never engages, and the wedge is something else -- the fix would be
/// inert and must not be credited. Non-zero means the loop really was capable of spinning
/// unboundedly while holding and re-taking `core` on every pass.
static POLL_SPIN_BREAKS: AtomicU64 = AtomicU64::new(0);
/// Non-blocking calls that fell through to the slow path -- the retry loop that livelocked.
///
/// Healthy rounds show ~10; every wedged round showed 80-90 million. Printed as `nbslow=` so the
/// amplifier is visible directly rather than inferred from `calls - fastok`.
static ENGINE_NB_SLOW: AtomicU64 = AtomicU64::new(0);
static ENGINE_FAST_OK: AtomicU64 = AtomicU64::new(0);
/// Entries to `blocking`, counted before the fast/slow branch, and wakeups returned from
/// `waiter.wait`. Both exist so that a zero in POLLS is a value someone measured rather than an
/// absence of output: the first trigger fires for any compartment that touches a socket at all,
/// the second separates "blocked and never woken" from "woken repeatedly, still not ready".
static ENGINE_CALLS: AtomicU64 = AtomicU64::new(0);
static ENGINE_WAKES: AtomicU64 = AtomicU64::new(0);
/// Arm selector for the wait-set change, so control and treatment differ by a constant in the
/// source rather than by a checkout.
///
/// Both arms must run on the *same* toolchain. The pending rustc swap makes `cfg(unix)` true for
/// this target, which changes which std source compiles -- `library/test`'s exit-status reporting
/// among it -- so any rate compared across that boundary is confounded, and the pre-swap
/// `pollsleep1` baseline (5 stalls in 12 rounds) cannot serve as the control for a post-swap fix.
///
/// `POLL_WAIT_COMPLETIONS = false` + `POLL_FALLBACK_MS = Some(50)` reproduces the pre-fix
/// behaviour exactly. A const rather than an env var deliberately: an environment arm is invisible
/// to `git diff` and to every mtime audit, which is the "flag flipped before your window" case
/// that no provenance check can see.
const POLL_WAIT_COMPLETIONS: bool = true;
/// `None`: the wait set is the whole set, so there is no periodic wake.
///
/// This was `Some(50)` as a backstop against a missed wake wedging a compartment forever, from
/// when `poll_delay()` returning `None` meant an unbounded sleep on a waiter the poll thread does
/// not control. It cost every compartment 20 wakeups a second whether or not anything was
/// happening -- work in exactly the case where there is none.
///
/// It can be dropped now because the two things it insured against are covered. The wait set
/// reads both queue sides it can be woken by (rx submissions via `recv_msg`, tx completions via
/// `check_completions`) plus `notify` for same-compartment changes, and a half-open connection --
/// previously the one state that could sit forever with no deadline -- now carries a socket
/// timeout that gives `poll_delay` a real deadline for exactly as long as the half-open exists
/// (see `LISTENER_HALF_OPEN_TIMEOUT`). Anything still missing is a hang the stall watchdog names,
/// which is the point: a missed wake should be a fault someone can see, not latency absorbed by a
/// timer that hides it.
const POLL_FALLBACK_MS: Option<u64> = None;

// Poll-thread wake accounting (diagnostic). A compartment that only *waits* -- any server --
// depends entirely on the rx waiter firing, because it has no socket calls of its own to wake
// its poll thread. These say whether that waiter ever does.
static POLL_ITERS: AtomicU64 = AtomicU64::new(0);
static POLL_SLEEPS: AtomicU64 = AtomicU64::new(0);
/// Sleeps that ran past their own bound, and the worst one in milliseconds.
///
/// The loop's sleep is `min(poll_delay, 50ms)`, so a poll thread cannot legitimately be away for
/// longer than that -- yet three of twelve rounds in `nbfix-0828b` show a peer's engine at
/// `polls <= 15` over a twenty-second window in which net-srv delivered it frames, and it then
/// woke on an explicit `wake()` at teardown. Either the sleep returned and the thread did not
/// loop, or the sleep did not return; nothing recorded which. These two say it directly, and a
/// healthy compartment is the instrument's own positive control: it idles on the 50ms fallback,
/// so `maxsleepms` near 50 with `overslept=0` proves the timing path is live and calibrated
/// rather than merely silent.
/// Watchdog ticks completed. Reported on every probe line so its liveness is a *number* on a
/// line the poll thread prints, not an absence of lines the watchdog failed to print. Absence
/// could not distinguish "no stall" from "this thread never ran", which is the trap that has
/// already cost three instruments today.
static WATCHDOG_TICKS: AtomicU64 = AtomicU64::new(0);
static POLL_OVERSLEPT: AtomicU64 = AtomicU64::new(0);
static POLL_MAX_SLEEP_MS: AtomicU64 = AtomicU64::new(0);
/// Where the poll thread is. Written only; the watchdog that read it was a diagnostic and is
/// stripped for timing arms (see prereg-mss-0827.md). A relaxed store is free enough to keep so
/// the next investigation does not have to re-thread it.
static POLL_PHASE: AtomicU64 = AtomicU64::new(0);
/// Wakes issued because a fast-path send queued egress. Separate from every other counter so that
/// "the fix engaged" is a measurement and not an inference from `polls` having moved -- `polls`
/// can rise for reasons that have nothing to do with this change.
static ENGINE_TXWAKE: AtomicU64 = AtomicU64::new(0);
/// Wakes issued after a `close()`/`abort()` on the paths that had none.
static ENGINE_CLOSEWAKE: AtomicU64 = AtomicU64::new(0);

/// Sleeps entered vs sleeps returned, and when the current one started.
///
/// `POLL_MAX_SLEEP_MS` is a `fetch_max` *after* the sleep returns, so it can only ever describe
/// sleeps that ended: the one sleep that matters -- the one still running -- is invisible to it.
/// `enter > exit` says a sleep is in flight right now, and `T0_MS` says for how long, so
/// "woke and did not progress" stops reading identically to "never woke".
static POLL_SLEEP_ENTER: AtomicU64 = AtomicU64::new(0);
static POLL_SLEEP_EXIT: AtomicU64 = AtomicU64::new(0);
/// Monotonic ms at the last sleep entry, published *before* the syscall.
static POLL_SLEEP_T0_MS: AtomicU64 = AtomicU64::new(0);
/// Timeout requested for the in-flight sleep, ms. `u64::MAX` = none requested.
static POLL_SLEEP_REQ_MS: AtomicU64 = AtomicU64::new(u64::MAX);
/// Watchdog `sleep()` entered vs returned. The watchdog is a 0-op timed `sys_thread_sync`; if it
/// stops returning, the compartment loses its only outside observer, so it needs its own pair.
static WD_SLEEP_ENTER: AtomicU64 = AtomicU64::new(0);
static WD_SLEEP_EXIT: AtomicU64 = AtomicU64::new(0);

/// Raw rx-ring state sampled where `any_ready` is computed, i.e. on the pass that decides whether
/// to sleep.
///
/// `has_rx_pending()` is `nonempty && turn`, and a false from it has two causes a bool cannot
/// separate: nothing was submitted, or entries are present with a turn bit the consumer does not
/// accept -- which also hides them from `receive`, so no wake would help. Published as the two
/// conjuncts plus the words behind them.
static MARK_READ_CALLS: AtomicU64 = AtomicU64::new(0);
static MARK_READ_RISE: AtomicU64 = AtomicU64::new(0);
/// When the most recent read-readiness rising edge was published (kernel-epoch ns). A reader
/// that measures its own wake against this splits "publication was late" from "the woken
/// thread was scheduled late" — see UDPRISE in smoltcp.rs.
pub(crate) static READ_RISE_LAST_NS: AtomicU64 = AtomicU64::new(0);
pub(crate) static RISE_CLOCK: twizzler_abi::syscall::FastClock =
    twizzler_abi::syscall::FastClock::new(
        twizzler_abi::syscall::ClockSource::BestMonotonic,
        twizzler_abi::syscall::ReadClockFlags::empty(),
    );
static MARK_READ_LEVEL: AtomicU64 = AtomicU64::new(0);
static MARK_NO_ENTRY: AtomicU64 = AtomicU64::new(0);
static RX_BELL: AtomicU64 = AtomicU64::new(0);
static RX_TAIL: AtomicU64 = AtomicU64::new(0);
static RX_NONEMPTY: AtomicU64 = AtomicU64::new(2);
static RX_TURN: AtomicU64 = AtomicU64::new(2);
// The tx submission ring's words, sampled alongside the rx set: the producer side of the
// .120->.122 stall split. `txbell > txtail` frozen = net-srv never consumed what this
// compartment submitted; `txbell == txtail` under an egress stall = this side never submitted.
static TX_BELL: AtomicU64 = AtomicU64::new(0);
static TX_TAIL: AtomicU64 = AtomicU64::new(0);
/// Entries to `TcpStreamInner::drop`, and the socket state seen there before `close()`.
///
/// The lost-FIN population's decisive question. net-srv's per-destination frame counters show a
/// failing round delivering exactly one frame to the peer and a passing round two, while the
/// parent's engine polls 149-224 times either way -- so a *queued* FIN would have gone out on some
/// pass, and the FIN is therefore never queued. That points at `close()` never running, i.e. this
/// `drop` never firing. `TCPDROPS` reading 0 for the parent in a failing round confirms it and
/// kills both rival explanations at once; any other value refutes it.
///
/// `TCPDROP_STATE` separates "drop never ran" from "drop ran on a socket in a state where close()
/// emits no FIN" -- different bugs with identical symptoms.
static TCPDROPS: AtomicU64 = AtomicU64::new(0);
static TCPDROP_STATE: AtomicU64 = AtomicU64::new(999);

/// Called from `TcpStreamInner::drop`, before `close()`.
/// One line per TCP close, keyed by endpoint, with state either side of `close()`.
///
/// Replaces the scalar `TCPDROP_STATE`, which was overwritten by every drop and therefore only
/// ever described the last of ~20 per boot. Single `sys_kernel_console_write` for the whole line:
/// `klog_println!` issues a syscall per fragment and klog interleaving splices the pieces, which
/// cost 75% of one earlier probe's records.
pub(super) fn note_tcp_close(
    lport: u16,
    raddr: core::net::IpAddr,
    rport: u16,
    before: State,
    after: State,
) {
    // Count before the gate: TCPDROPS feeds the POLLPROBE line and must not stop when the
    // per-close print is off.
    let n = TCPDROPS.fetch_add(1, Ordering::Relaxed) + 1;
    if !twizzler_net::diag_enabled("net") {
        return;
    }
    use core::fmt::Write;
    struct Line {
        b: [u8; 256],
        n: usize,
    }
    impl Write for Line {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let end = (self.n + s.len()).min(self.b.len());
            self.b[self.n..end].copy_from_slice(&s.as_bytes()[..end - self.n]);
            self.n = end;
            Ok(())
        }
    }
    let mut line = Line { b: [0; 256], n: 0 };
    let _ = writeln!(
        line,
        "TCPCLOSE octet={} lport={} raddr={} rport={} before={:?} after={:?} n={}",
        ENGINE_OCTET.load(Ordering::Relaxed),
        lport,
        raddr,
        rport,
        before,
        after,
        n,
    );
    twizzler_abi::syscall::sys_kernel_console_write(
        twizzler_abi::syscall::KernelConsoleSource::Console,
        &line.b[..line.n],
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
}

/// Wake the poll thread after a socket state change that queued egress.
///
/// `close()` queues a FIN and `abort()` a RST; neither transmits. The poll thread does, and it is
/// asleep under a deadline `poll_delay` returned *before* that segment existed -- `None` for an
/// otherwise-idle established socket, so the sleep has no timeout at all. Same stale-deadline
/// defect as the send fast path (see `blocking_egress`), at the sites that bypass `blocking`
/// entirely by taking `core` directly.
///
/// `TcpStreamInner::drop` is deliberately not a caller: it already wakes.
///
/// Unconditional, unlike `blocking`: close/shutdown/drop are not the hot send path. Callers must
/// drop the `core` guard first -- `wake()` is a syscall, and holding the mutex across it
/// serialises every socket operation in the compartment behind the poll thread waking up.
pub(super) fn wake_after_close() {
    ENGINE_CLOSEWAKE.fetch_add(1, Ordering::Relaxed);
    ENGINE.wake();
}

pub(super) fn note_tcp_drop(state: State) {
    TCPDROP_STATE.store(
        match state {
            State::Closed => 0,
            State::Listen => 1,
            State::SynSent => 2,
            State::SynReceived => 3,
            State::Established => 4,
            State::FinWait1 => 5,
            State::FinWait2 => 6,
            State::CloseWait => 7,
            State::Closing => 8,
            State::LastAck => 9,
            State::TimeWait => 10,
        },
        Ordering::Relaxed,
    );
    let n = TCPDROPS.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_power_of_two() {
        pollprobe("tcpdrop");
    }
}
/// Last octet of this compartment's own address, so the console line names who it belongs to --
/// every compartment shares one console, and `.106` is the address the net-srv counter already
/// keys on.
static ENGINE_OCTET: AtomicU64 = AtomicU64::new(999);

/// One line shape for every emit site.
///
/// The first version of this probe gated its report on `ENGINE_FAST_OK`, i.e. on a sibling of the
/// quantity under test. A compartment that never reached the fast path printed nothing, so
/// "never polled" and "polled normally, never fast-pathed" -- opposite answers -- produced the
/// same silence, and the probe went quiet in exactly the population it was built to describe.
/// Each counter now triggers on its own value and every line carries all four, so any one site
/// firing turns the other three zeros into measurements.
static GROUP_NOTREADY: AtomicU64 = AtomicU64::new(0);

/// One line naming the backlog's per-state census when a listener group is not ready.
fn groupcensus(site: &str, n: u64, c: &[u16; 11]) {
    if !twizzler_net::diag_enabled("net") {
        return;
    }
    use core::fmt::Write;
    struct Line {
        b: [u8; 256],
        n: usize,
    }
    impl Write for Line {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let end = (self.n + s.len()).min(self.b.len());
            self.b[self.n..end].copy_from_slice(&s.as_bytes()[..end - self.n]);
            self.n = end;
            Ok(())
        }
    }
    let mut line = Line { b: [0; 256], n: 0 };
    let _ = writeln!(
        line,
        "GROUPCENSUS octet={} src={} iters={} n={} closed={} listen={} synsent={} synrecv={} estab={} finw1={} finw2={} closewait={} closing={} lastack={} timewait={}",
        ENGINE_OCTET.load(Ordering::Relaxed),
        site,
        POLL_ITERS.load(Ordering::Relaxed),
        n,
        c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], c[8], c[9], c[10]
    );
    twizzler_abi::syscall::sys_kernel_console_write(
        twizzler_abi::syscall::KernelConsoleSource::Console,
        &line.b[..line.n],
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
}

/// State of the engine core as last sampled from outside the poll thread. -1 = not sampled,
/// 0 = free and nothing queued, 1 = free with rx queued, 2 = locked, 3 = poisoned.
static EXT_CORE: AtomicU64 = AtomicU64::new(u64::MAX);

/// Where inside `Core::poll` the poll thread last was.
///
/// `POLL_PHASE` resolves only to "somewhere inside `inner.poll`", and the frozen signature is
/// `polls=8 iters=7 sleeps=6 phase=3` -- *identical* across every instance in both arms of the
/// nonblock A/B. A timing race scatters those counts; the same three numbers every time is a state
/// machine stopping at one statement. This splits that statement apart so the count names a line.
static POLL_SUB: AtomicU64 = AtomicU64::new(0);

/// Arm selector: emit the three probes that run **while the engine core mutex is held**.
///
/// `pollprobe` ends in `sys_kernel_console_write`. Three of its call sites -- "poll", "fast",
/// "wake" -- are inside the lock, and the "poll" one is gated on `is_power_of_two()`, so it fires
/// on exactly the 1st, 2nd, 4th, 8th... poll. Every frozen peer this session stopped at
/// `polls=8 iters=7 sleeps=6`, and `sub=1` with `phase=3` places the thread between
/// `POLL_PHASE.store(3)` and the first `POLL_SUB.store(10)` -- a gap whose only substantial
/// content is that console write. A constant rather than a distribution is what a fixed-count gate
/// predicts and a race does not.
///
/// There is prior measurement for the pattern in this same subsystem: `net-srv`'s `deliver_local`
/// records that a probe logging under its locks "took the suite from 13/50 failures to 50/50".
///
/// `false` removes only the in-lock probes; every probe outside the lock stays, so the failure
/// count is still readable from sysbench's own markers.
/// 2026-08-30: set `false`. Two reasons, and the second matters more than the first.
/// A synchronous console write inside the engine's core mutex perturbs the subsystem being
/// measured -- 87 fired inside the lock in one bench window. And per the analysis above it is a
/// candidate *cause* of the peer freeze, not merely noise, so a measurement taken with it on
/// cannot distinguish the bug from the instrument. Flip it back to reproduce that hypothesis
/// deliberately; do not leave it on for baselines.
/// Whether the stall watchdog thread runs. See its spawn site: a wakeup every 2s per network
/// compartment, forever, purely to diagnose a poll-thread stall.
const STALL_WATCHDOG: bool = false;

const PROBE_UNDER_LOCK: bool = false;

/// Emit the engine's liveness counters from whatever thread calls this, plus the one thing the
/// poll thread cannot report about itself: whether it is wedged, and whether frames are waiting.
///
/// Called from `SmolTcpListener::waitpoint`, i.e. from a thread that is provably still running --
/// the only vantage point from which a stopped engine can be described at all. A counter the poll
/// thread increments cannot distinguish "stopped" from "never started"; this can.
///
/// `try_lock`, never `lock`: blocking here would turn one wedged thread into two and destroy the
/// reporter along with the subject. Failing to take it is not an error, it *is* a reading -- it
/// means the poll thread is holding the core mutex, which is the blocking-queue-submit-under-the-
/// lock case (live whenever `NONBLOCK_POLL_QUEUE` is false). The three outcomes are three
/// different bugs:
///
///   core=locked        -- poll thread wedged inside `Core::poll` holding the mutex
///   core=free rxpend=1 -- frames delivered and sitting unconsumed; an engine wait/wake defect
///   core=free rxpend=0 -- nothing queued; the frames never reached this client at all
pub(super) fn report_engine_liveness() {
    let state = match ENGINE.core.try_lock() {
        Ok(core) => {
            if core.ifaceset.iter().any(|i| i.device.has_rx_pending()) {
                1
            } else {
                0
            }
        }
        Err(std::sync::TryLockError::WouldBlock) => 2,
        Err(_) => 3,
    };
    EXT_CORE.store(state, Ordering::Relaxed);
    // Fire on the condition, not on a power-of-two schedule. The gated schedule gave each failing
    // peer four lines across its entire 20s life, which is why the freeze had to be inferred from
    // counters rather than observed. A sleep in flight for more than twice its own requested
    // timeout (floor 1s) is the anomaly itself, so report it every time it is true.
    let stuck = sleep_inflight_ms().is_some_and(|age| {
        let req = POLL_SLEEP_REQ_MS.load(Ordering::Relaxed);
        age > req.saturating_mul(2).max(1000)
    });
    if stuck {
        pollprobe("SLEEPSTUCK");
        return;
    }
    pollprobe("extern");
}

/// Time each poll-loop pass spends acquiring-plus-holding `core` (section 0 = the smoltcp poll,
/// section 1 = the census/waiter-assembly pass). The kevent bookkeeping path re-checks levels
/// through this same lock; if these run long, a woken waiter's readiness report convoys here.
fn core_held_note(section: usize, d: std::time::Duration) {
    static SUM: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    static CNT: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    static MAX: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    let ns = d.as_nanos() as u64;
    SUM[section].fetch_add(ns, Ordering::Relaxed);
    MAX[section].fetch_max(ns, Ordering::Relaxed);
    let n = CNT[section].fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_power_of_two() && twizzler_net::diag_enabled("net") {
        println!(
            "POLLHELD s{} n={} avg_us={} max_us={}",
            section,
            n,
            SUM[section].load(Ordering::Relaxed) / n / 1000,
            MAX[section].load(Ordering::Relaxed) / 1000
        );
    }
}

/// Refresh the ring-word statics the probe line reports, straight from the shared queue headers.
///
/// The poll loop samples them once per census pass, which is exactly the thread that stops moving
/// in a stall -- so a probe fired from anywhere else would print words as old as the stall itself.
/// `try_lock`, never `lock`: this is called from kevent's timeout path, which must not join a
/// convoy on `core` (and must not deadlock if a future call site already holds it). A failed try
/// leaves the last sample in place, which is the pre-existing behaviour.
pub(crate) fn sample_rings() {
    let Ok(core) = ENGINE.core.try_lock() else {
        return;
    };
    if let Some(iface) = core.ifaceset.iter().next() {
        let (bell, tail, nonempty, turn) = iface.device.rx_pending_parts();
        RX_BELL.store(bell, Ordering::Relaxed);
        RX_TAIL.store(tail, Ordering::Relaxed);
        RX_NONEMPTY.store(nonempty as u64, Ordering::Relaxed);
        RX_TURN.store(turn as u64, Ordering::Relaxed);
        let (tbell, ttail, _, _) = iface.device.tx_pending_parts();
        TX_BELL.store(tbell, Ordering::Relaxed);
        TX_TAIL.store(ttail, Ordering::Relaxed);
    }
}

pub(crate) fn pollprobe(site: &str) {
    // Off unless the boot line asked for it (`--diag=net` -> TWZ_DIAG): every site funnels
    // through here, including per-waitpoint "extern" probes that fire on ordinary interactive
    // socket use. The counters it reports keep accumulating regardless.
    if !twizzler_net::diag_enabled("net") {
        return;
    }
    // One console write for the whole line, rather than `klog_println!`.
    //
    // `klog_println!` drives `core::fmt` straight at `sys_kernel_console_write`, which issues a
    // separate syscall per literal fragment and per argument -- thirteen for the line below. The
    // serial lock is only held for the duration of one call, so with a dozen compartments logging
    // at once the console spliced them character by character: `POLLPROBE octet=2215 fastok=` is
    // two lines interleaved, and ~70% of probe lines in the first smoke sweep arrived
    // unparseable. The parser demands all six fields, so those were discarded rather than
    // misread -- but the loss is heaviest exactly when many compartments start at once, which is
    // when a compartment most needs to prove it exists. That turns splicing into another source
    // of the false silence this probe exists to eliminate.
    //
    // Buffer overflow truncates, and a truncated line fails the parser's full-field match, so the
    // failure direction stays "discarded", never "plausible wrong number".
    use core::fmt::Write;
    struct Line {
        b: [u8; 512],
        n: usize,
    }
    impl Write for Line {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let end = (self.n + s.len()).min(self.b.len());
            self.b[self.n..end].copy_from_slice(&s.as_bytes()[..end - self.n]);
            self.n = end;
            Ok(())
        }
    }
    let mut line = Line { b: [0; 512], n: 0 };
    let _ = writeln!(
        line,
        // `sctx` joins this line to the kernel's `[hang]` records, which carry the same id. Without
        // it the two instruments describe the same frozen compartment in vocabularies that cannot
        // be matched up -- octet on one side, thread ids on the other.
        "POLLPROBE octet={} sctx={:x} site={} extcore={} sub={} calls={} fastok={} polls={} wakes={} txwake={} closewake={} tcpdrops={} dropstate={} spinbreaks={} nbslow={} iters={} sleeps={} phase={} overslept={} maxsleepms={} wdticks={} slpin={} slpout={} slpage={} slpreq={} wdin={} wdout={} engms={} rxbell={} rxtail={} rxne={} rxturn={} udpacc={} devtx={} mkcalls={} mkrise={} mklevel={} mknoent={} txbell={} txtail={} ringw={} ringnw={}",
        ENGINE_OCTET.load(Ordering::Relaxed),
        secgate::get_sctx_id().raw(),
        site,
        EXT_CORE.load(Ordering::Relaxed) as i64,
        POLL_SUB.load(Ordering::Relaxed),
        ENGINE_CALLS.load(Ordering::Relaxed),
        ENGINE_FAST_OK.load(Ordering::Relaxed),
        ENGINE_POLLS.load(Ordering::Relaxed),
        ENGINE_WAKES.load(Ordering::Relaxed),
        ENGINE_TXWAKE.load(Ordering::Relaxed),
        ENGINE_CLOSEWAKE.load(Ordering::Relaxed),
        TCPDROPS.load(Ordering::Relaxed),
        TCPDROP_STATE.load(Ordering::Relaxed),
        POLL_SPIN_BREAKS.load(Ordering::Relaxed),
        ENGINE_NB_SLOW.load(Ordering::Relaxed),
        POLL_ITERS.load(Ordering::Relaxed),
        POLL_SLEEPS.load(Ordering::Relaxed),
        POLL_PHASE.load(Ordering::Relaxed),
        POLL_OVERSLEPT.load(Ordering::Relaxed),
        POLL_MAX_SLEEP_MS.load(Ordering::Relaxed),
        WATCHDOG_TICKS.load(Ordering::Relaxed),
        POLL_SLEEP_ENTER.load(Ordering::Relaxed),
        POLL_SLEEP_EXIT.load(Ordering::Relaxed),
        // -1 rather than 0 for "no sleep in flight": 0 is a legitimate age, and a field that
        // renders both as the same number is the defect this whole probe exists to avoid.
        sleep_inflight_ms().map(|v| v as i64).unwrap_or(-1),
        POLL_SLEEP_REQ_MS.load(Ordering::Relaxed) as i64,
        WD_SLEEP_ENTER.load(Ordering::Relaxed),
        WD_SLEEP_EXIT.load(Ordering::Relaxed),
        // Engine age. Separates the two live hypotheses: ~20000 means the engine was built at
        // bind and its poll thread then did almost nothing (starvation); ~400 means the engine
        // itself was only constructed moments ago (late start). Zeroed at `pollprobe("init")`.
        mono_ms(),
        RX_BELL.load(Ordering::Relaxed),
        RX_TAIL.load(Ordering::Relaxed),
        RX_NONEMPTY.load(Ordering::Relaxed),
        RX_TURN.load(Ordering::Relaxed),
        twizzler_net::UDP_SEND_ACCEPTED.load(Ordering::Relaxed),
        twizzler_net::DEV_TX_FRAMES.load(Ordering::Relaxed),
        MARK_READ_CALLS.load(Ordering::Relaxed),
        MARK_READ_RISE.load(Ordering::Relaxed),
        MARK_READ_LEVEL.load(Ordering::Relaxed),
        MARK_NO_ENTRY.load(Ordering::Relaxed),
        TX_BELL.load(Ordering::Relaxed),
        TX_TAIL.load(Ordering::Relaxed),
        twizzler_net::RING_WOKE.load(Ordering::Relaxed),
        twizzler_net::RING_NO_WAITER.load(Ordering::Relaxed),
    );
    twizzler_abi::syscall::sys_kernel_console_write(
        twizzler_abi::syscall::KernelConsoleSource::Console,
        &line.b[..line.n],
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
}

lazy_static::lazy_static! {
    static ref ENGINE_T0: std::time::Instant = std::time::Instant::now();
}

fn mono_ms() -> u64 {
    ENGINE_T0.elapsed().as_millis() as u64
}

/// Age of the in-flight sleep in ms, or `None` if no sleep is currently in flight.
fn sleep_inflight_ms() -> Option<u64> {
    // ENTER must be read before EXIT, and swapping these two operands is the one edit that breaks
    // this function. Read ENTER first: a sleep that completes between the two loads then yields
    // enter == exit -- stale by one sample, reported as "not in flight", harmless. Read EXIT first
    // and the same interleaving yields an old EXIT against a new ENTER, i.e. a *false in-flight*
    // for a sleep that already returned -- which is precisely the "stuck sleep" this exists to
    // detect, manufactured by the detector. Only one order fails safe.
    if POLL_SLEEP_ENTER.load(Ordering::Relaxed) == POLL_SLEEP_EXIT.load(Ordering::Relaxed) {
        return None;
    }
    Some(mono_ms().saturating_sub(POLL_SLEEP_T0_MS.load(Ordering::Relaxed)))
}

lazy_static::lazy_static! {
    pub(crate) static ref ENGINE: Arc<Engine> = Arc::new(Engine::new());
    pub(crate) static ref WAITERS: Arc<Waiters> = Arc::new(Waiters::default());
}

struct Wait {
    read: Arc<AtomicU64>,
    write: Arc<AtomicU64>,
    // Monotonic count of falling edges (ready -> not ready) on each side, never reset. An
    // edge-triggered consumer samples this when it suppresses a readiness it has already
    // reported, and re-arms once it moves. That is the piece the level words cannot express:
    // they only say what is true *now*, so a drain followed by a refill while nobody was
    // looking is indistinguishable from the readiness never having gone away -- which would
    // leave an edge-triggered waiter suppressed forever with data pending.
    read_down: Arc<AtomicU64>,
    write_down: Arc<AtomicU64>,
}

impl Wait {
    // Both level words start "not ready" (0). Claiming readiness before the engine thread has
    // ever observed this socket would make poll/select/kevent report a bogus immediate
    // ready; the live smoltcp check (SocketKind::is_ready, OR'd in by every consumer)
    // covers the window until the first Core::poll pass.
    pub fn new() -> Self {
        Self {
            read: Arc::new(AtomicU64::new(0)),
            write: Arc::new(AtomicU64::new(0)),
            read_down: Arc::new(AtomicU64::new(0)),
            write_down: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[derive(Default)]
pub(crate) struct Waiters {
    map: Mutex<HashMap<WaitKey, Wait>>,
    groups: Mutex<GroupIds>,
}

#[derive(Default)]
struct GroupIds {
    next: u64,
    free: Vec<u64>,
}

impl Waiters {
    /// Mint an identity for a new listener group. Ids are recycled, so the map stays bounded by the
    /// peak number of concurrently live groups and a `Wait`, once created, is never freed -- the
    /// same contract init_waiter documents, for the same reason.
    pub fn alloc_group(&self) -> u64 {
        let id = {
            let mut ids = self.groups.lock().unwrap();
            match ids.free.pop() {
                Some(id) => id,
                None => {
                    let id = ids.next;
                    ids.next += 1;
                    id
                }
            }
        };
        self.init_waiter(WaitKey::Group(id));
        id
    }

    pub fn free_group(&self, id: u64) {
        self.groups.lock().unwrap().free.push(id);
    }

    // Returns an owning clone of the wait word rather than just a raw pointer into it. The
    // underlying allocation is in practice permanently stable once created (see
    // init_waiter's comment), but callers that can retain the clone (kqueue/poll/select)
    // should still do so -- it costs nothing and keeps this API's contract independent of
    // that implementation detail.
    pub fn waitpoint(
        &self,
        key: WaitKey,
        kind: wait_kind,
    ) -> Result<(Arc<AtomicU64>, u64), TwzError> {
        let mut map = self.map.lock().unwrap();
        let entry = map.entry(key).or_insert_with(|| Wait::new());
        let arc = match kind {
            x if x == WAIT_READ => entry.read.clone(),
            x if x == WAIT_WRITE => entry.write.clone(),
            _ => return Err(TwzError::INVALID_ARGUMENT),
        };
        Ok((arc, 0))
    }

    /// The falling-edge counter for `kind`, plus its current value. Sleeping on it with that
    /// value blocks until the next ready -> not-ready transition; retaining the value and
    /// comparing later tells you whether one has happened since.
    pub fn down_waitpoint(
        &self,
        key: WaitKey,
        kind: wait_kind,
    ) -> Result<(Arc<AtomicU64>, u64), TwzError> {
        let mut map = self.map.lock().unwrap();
        let entry = map.entry(key).or_insert_with(|| Wait::new());
        let arc = match kind {
            x if x == WAIT_READ => entry.read_down.clone(),
            x if x == WAIT_WRITE => entry.write_down.clone(),
            _ => return Err(TwzError::INVALID_ARGUMENT),
        };
        let val = arc.load(Ordering::SeqCst);
        Ok((arc, val))
    }

    fn mark_waiter(&self, key: WaitKey, read: bool, write: bool) {
        // Accounting for the one path that can silently publish nothing: a key absent from the map
        // updates no word and sends no wake, and every downstream counter would look identical to
        // "the socket was simply not ready". MARK_READ_LEVEL is the rising-edge design's blind
        // spot -- readiness re-asserted while already 1 sends no wake, which is only safe because
        // a later arm re-reads the level.
        if read {
            MARK_READ_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        let map = self.map.lock().unwrap();
        if read && map.get(&key).is_none() {
            MARK_NO_ENTRY.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(wait) = map.get(&key) {
            let mut rfell = false;
            let mut wfell = false;
            let rwake = if read {
                let rose = wait.read.swap(1, Ordering::SeqCst) == 0;
                if rose {
                    MARK_READ_RISE.fetch_add(1, Ordering::Relaxed);
                    READ_RISE_LAST_NS
                        .store(RISE_CLOCK.get().as_nanos() as u64, Ordering::Relaxed);
                } else {
                    MARK_READ_LEVEL.fetch_add(1, Ordering::Relaxed);
                }
                rose
            } else {
                rfell = wait.read.swap(0, Ordering::SeqCst) == 1;
                if rfell {
                    wait.read_down.fetch_add(1, Ordering::SeqCst);
                }
                false
            };
            let wwake = if write {
                wait.write.swap(1, Ordering::SeqCst) == 0
            } else {
                wfell = wait.write.swap(0, Ordering::SeqCst) == 1;
                if wfell {
                    wait.write_down.fetch_add(1, Ordering::SeqCst);
                }
                false
            };

            // Only wake a side that actually transitioned not-ready -> ready. Waking
            // unconditionally turns the rare spurious wakeup that sys_thread_sync callers must
            // tolerate into a per-poll-cycle event, which breaks the single-shot
            // poll/select/kevent waits (they would return early with nothing ready).
            //
            // A falling edge is a wakeup too: an edge-triggered consumer suppressed on this side
            // is blocked on the down counter waiting for exactly this.
            //
            // Stack-allocated: this runs once per waiter per poll cycle, and the common case
            // pushes nothing at all, so a Vec here was a heap allocation per socket per poll for
            // a list that can never exceed four entries.
            let mut wakes = [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&*wait.read),
                usize::MAX,
            )); 4];
            let mut n = 0;
            for (fired, word) in [
                (rwake, &wait.read),
                (wwake, &wait.write),
                (rfell, &wait.read_down),
                (wfell, &wait.write_down),
            ] {
                if fired {
                    wakes[n] = ThreadSync::new_wake(ThreadSyncWake::new(
                        ThreadSyncReference::Virtual(&**word),
                        usize::MAX,
                    ));
                    n += 1;
                }
            }
            if n > 0 {
                let _ = sys_thread_sync(&mut wakes[..n], None);
            }
        }
    }

    // Resets (rather than replaces) any existing entry for `key`, so the underlying AtomicU64
    // allocation for a given key value, once created, is never freed for the life of the
    // process -- it is only ever reset back to Wait::new()'s initial state. This keeps a raw
    // `*const AtomicU64` handed out for this key (e.g. across the extern "C"
    // twz_rt_fd_waitpoint ABI, which cannot carry an owning Arc back to its caller -- see
    // ReferenceRuntime::fd_waitpoint) permanently valid even after the socket or group behind
    // it is released and its identity reused. The map can only grow to the peak number of
    // concurrently live keys: smoltcp's SocketSet reuses freed handles rather than handing out
    // ever-increasing values, and alloc_group recycles group ids for the same reason.
    //
    // The falling-edge counters are deliberately not reset: they are monotonic for the life of
    // the process, so a suppression token held over an identity reuse can only ever read as
    // "it moved" (a spurious re-arm, harmless) and never as "it did not" (a silent hang).
    fn init_waiter(&self, key: WaitKey) {
        let mut map = self.map.lock().unwrap();
        let wait = map.entry(key).or_insert_with(Wait::new);
        wait.read.store(0, Ordering::SeqCst);
        wait.write.store(0, Ordering::SeqCst);
    }
}

impl Engine {
    fn new() -> Self {
        let (iface, device) = get_twznet_device_and_interface();

        let nc_handle = device.info.handle;
        let mut nic = IfaceSet::new(device);
        nic.insert_iface(iface);

        let core = Arc::new(Mutex::new(Core::new(vec![nic])));
        let waiter = Arc::new(Condvar::new());
        let notify = Arc::new(AtomicU64::new(0));
        let _inner = core.clone();
        let _waiter = waiter.clone();
        let _notify = notify.clone();

        // Okay, here is our background polling thread. It polls the network interface with the
        // SocketSet whenever it needs to, which is:
        // 1. when smoltcp says to based on poll_time() (calls poll_delay internally)
        // 2. when the state changes (eg a new socket is added)
        // 3. when blocking threads need to poll (we get a message on the channel)
        let thread = std::thread::spawn(move || {
            let inner = _inner;
            let waiter = _waiter;
            let notify = _notify;

            fn check_tracking() {
                if TRACKING_LEN.load(Ordering::Relaxed) == 0 {
                    return;
                }
                // Collected under the lock, returned after it: return_port takes the port
                // allocator's lock, and taking that while holding `core` would invert the order
                // the original established by dropping `core` first.
                let mut ports: Vec<u16> = Vec::new();
                {
                    let mut core = ENGINE.core.lock().unwrap();
                    let mut idx = 0;
                    while idx < core.tracking.len() {
                        let item = core.tracking[idx];
                        // A handle already gone from the set has nothing left to release, and
                        // get_mutable_socket would panic on it.
                        let gone = !core.live.contains(&item.0);
                        let is_closed = gone
                            || match item.2 {
                                SockKind::Tcp => {
                                    core.get_mutable_socket(item.0).state() == State::Closed
                                }
                                // TODO: this causes some kind of stall.
                                SockKind::Udp => false, /* not core.get_mutable_udp_socket(item.
                                                         * 0).
                                                         * is_open(), */
                            };
                        if is_closed {
                            if !gone {
                                core.release_socket(item.0);
                            }
                            core.tracking.remove(idx);
                            TRACKING_LEN.fetch_sub(1, Ordering::Relaxed);
                            // `track` stores 0 for a socket whose port it does not own (see
                            // Engine::track); returning it would decrement a refcount net-srv
                            // never incremented.
                            if item.1 != 0 {
                                ports.push(item.1);
                            }
                        } else {
                            idx += 1;
                        }
                    }
                }
                // Bracket the secgate rather than inferring its state from elsewhere: `return_port`
                // calls into net-srv, so a poll thread stopped here is stopped in another
                // compartment, and no counter in this one can say so.
                //
                // (An earlier version of this comment claimed phase 3 was stored inside the core
                // block above. It is not -- phase 3 is set later, around `inner.poll`. The wrong
                // claim was load-bearing for a hypothesis that cost a sweep, so it is corrected
                // here rather than deleted.)
                //
                //   8 = core released, nothing left to return
                //   6 = inside `return_port`, i.e. blocked in the secgate into net-srv
                //   7 = a return completed, going round again
                POLL_PHASE.store(8, Ordering::Relaxed);
                for port in ports {
                    POLL_PHASE.store(6, Ordering::Relaxed);
                    ENGINE.return_port(port);
                    POLL_PHASE.store(7, Ordering::Relaxed);
                }
            }

            // Refilled, not rebuilt: the set changes only when an interface comes or goes, but the
            // loop below runs on every poll cycle.
            let mut waiters: Vec<ThreadSync> = Vec::new();
            /// How many consecutive immediate re-polls before the loop is made to sleep once.
            /// High enough that genuine back-to-back work is unaffected; low enough that a
            /// livelock yields in well under a millisecond.
            const MAX_IMMEDIATE_REPOLLS: u32 = 1000;
            let mut immediate_repolls: u32 = 0;
            loop {
                POLL_PHASE.store(1, Ordering::Relaxed);
                // Reset each iteration. Without this a `sub` left over from the previous poll
                // reads exactly like a thread stopped at that point -- which is how `sub=15`
                // ("ifaceset loop finished, nothing ready") got mistaken for a position when it
                // was simply the last thing the previous poll wrote. A stale reading that looks
                // current is the defect this whole probe exists to avoid.
                POLL_SUB.store(1, Ordering::Relaxed);
                check_tracking();
                POLL_PHASE.store(2, Ordering::Relaxed);
                let __t_held = std::time::Instant::now();
                let time = {
                    let mut inner = inner.lock().unwrap();
                    POLL_PHASE.store(3, Ordering::Relaxed);
                    inner.poll(&*waiter);
                    POLL_SUB.store(20, Ordering::Relaxed);
                    let time = inner.poll_time();
                    POLL_SUB.store(21, Ordering::Relaxed);

                    // We may need to poll immediately!
                    //
                    // **Bounded -- as hardening for a latent hazard, NOT as a fix for any
                    // observed wedge.** This `continue` was unconditional, which makes the loop a
                    // hard spin if smoltcp ever keeps naming a sub-100us deadline: no sleep, no
                    // yield, `core` re-taken every pass, starving every other socket operation in
                    // the compartment. It also skips `POLL_ITERS`, so such a spin would be
                    // invisible to the loop's own counters.
                    //
                    // I hypothesised that this caused the net-throughput wedge and added
                    // `POLL_SPIN_BREAKS` to test it. **It read 0 across 3459 samples in a 12-round
                    // sweep: the bound has never once engaged, so the loop does not spin in
                    // practice and this is not the cause of anything.** Do not credit it for a
                    // wedge-rate change; the measured improvement belongs to the bounded reap in
                    // sysbench's `EchoPeer::shutdown` and the peer-liveness check in the bench
                    // body. If the counter is ever seen non-zero, that is new information.
                    //
                    // Falling through costs one bounded sleep -- `timeout` below is
                    // `time.min(50ms)`, i.e. <100us here -- so the fast path keeps its speed
                    // (one syscall per MAX_IMMEDIATE_REPOLLS polls) and a livelock becomes a
                    // yield instead of a hang.
                    if time.is_some_and(|time| time.total_micros() < 100) {
                        POLL_SUB.store(22, Ordering::Relaxed);
                        inner.poll(&*waiter);
                        POLL_SUB.store(23, Ordering::Relaxed);
                        immediate_repolls += 1;
                        if immediate_repolls < MAX_IMMEDIATE_REPOLLS {
                            core_held_note(0, __t_held.elapsed());
                            continue;
                        }
                        immediate_repolls = 0;
                        POLL_SPIN_BREAKS.fetch_add(1, Ordering::Relaxed);
                    } else {
                        POLL_SUB.store(24, Ordering::Relaxed);
                        immediate_repolls = 0;
                    }
                    POLL_SUB.store(25, Ordering::Relaxed);
                    time
                };
                core_held_note(0, __t_held.elapsed());

                POLL_PHASE.store(4, Ordering::Relaxed);
                let __t_held2 = std::time::Instant::now();
                let core = inner.lock().unwrap();
                // Time-spaced, not event-spaced: a stalled listener produces no events, which is
                // exactly when its state matters.
                if POLL_ITERS.load(Ordering::Relaxed) % 64 == 0 {
                    core.census_groups();
                }
                waiters.clear();
                // Both directions, not just rx. A wait set that misses one of its wake reasons is
                // what makes a fallback timeout load-bearing instead of merely a safety net.
                for iface in core.ifaceset.iter() {
                    waiters.push(ThreadSync::new_sleep(iface.device.rx_waiter()));
                    if POLL_WAIT_COMPLETIONS {
                        waiters.push(ThreadSync::new_sleep(iface.device.tx_completions_waiter()));
                    }
                    // Self-gating: `None` unless a completion is actually owed, which cannot
                    // happen in the blocking control arm.
                    if let Some(w) = iface.device.rx_completion_space_waiter() {
                        waiters.push(ThreadSync::new_sleep(w));
                    }
                }
                waiters.push(ThreadSync::new_sleep(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(&*notify),
                    0,
                    twizzler_abi::syscall::ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                )));

                let any_ready = core
                    .ifaceset
                    .iter()
                    .any(|iface| iface.device.has_rx_pending());
                // Sampled here, not in `pollprobe`: this is the pass that decides to sleep, and the
                // question is what the ring looked like at that decision rather than whenever a
                // probe next fires.
                if let Some(iface) = core.ifaceset.iter().next() {
                    let (bell, tail, nonempty, turn) = iface.device.rx_pending_parts();
                    RX_BELL.store(bell, Ordering::Relaxed);
                    RX_TAIL.store(tail, Ordering::Relaxed);
                    RX_NONEMPTY.store(nonempty as u64, Ordering::Relaxed);
                    RX_TURN.store(turn as u64, Ordering::Relaxed);
                    let (tbell, ttail, _, _) = iface.device.tx_pending_parts();
                    TX_BELL.store(tbell, Ordering::Relaxed);
                    TX_TAIL.store(ttail, Ordering::Relaxed);
                }
                drop(core);
                core_held_note(1, __t_held2.elapsed());
                let n = notify.swap(0, Ordering::SeqCst);
                POLL_ITERS.fetch_add(1, Ordering::Relaxed);
                if !any_ready && n == 0 {
                    POLL_SLEEPS.fetch_add(1, Ordering::Relaxed);
                    POLL_PHASE.store(5, Ordering::Relaxed);
                    // A bounded fallback when smoltcp has no deadline of its own. `poll_delay`
                    // returns None for an idle compartment, which made this an *unbounded* sleep
                    // on the rx waiter -- so a single missed wake wedged the compartment
                    // permanently rather than delaying it. Measured (udpdiag2): the UDP peer's
                    // poll thread ran five iterations and its last sleep never returned while 128
                    // frames sat queued for it. A poll thread that can only be woken by a waiter
                    // it does not control should not stake liveness on that waiter being perfect.
                    // Capped, not just defaulted. Two separate ways this sleep could run long:
                    // `poll_delay()` returns None for an idle compartment (unbounded sleep), and
                    // it can also return a *large* deadline when no socket has near-term work.
                    // Either way the thread's liveness rests entirely on a waiter it does not
                    // control, which is why a UDP echo server parked in kevent never woke while
                    // 128 frames sat queued for it. Capping makes a missed wake cost <=50ms
                    // instead of costing the compartment.
                    //
                    // The `missed` counter that used to sit after this sleep is gone with the rest
                    // of the SLEEPPROBE diagnostics (stripped for timing arms, prereg-mss-0827.md)
                    // and should not be restored as it was: `rx pending && !notified` is equally
                    // the signature of the rx waiter firing *correctly*, so it could not tell a
                    // dropped wake from a working one. Time the sleep and compare against
                    // FALLBACK if that question is asked again.
                    // smoltcp's own deadline, or nothing. There is no fallback floor any more.
                    //
                    // A poll thread that wakes periodically "in case it missed something" cannot
                    // tell a complete wait set from a broken one: every missed wake becomes
                    // latency rather than a fault, permanently invisible, and it was the only
                    // reason `Pair::progress` had to exist. The wait set above is now the whole
                    // set the client can be woken by -- it reads exactly two queue sides, rx
                    // submissions via `recv_msg` and tx completions via `check_completions`, and
                    // both are in it, plus `notify` for same-compartment state changes. Anything
                    // still missing is now a hang the stall watchdog names, which is the point.
                    let timeout: Option<std::time::Duration> = match POLL_FALLBACK_MS {
                        Some(ms) => {
                            let cap = std::time::Duration::from_millis(ms);
                            Some(time.map(|t| t.into()).unwrap_or(cap).min(cap))
                        }
                        None => time.map(|t| t.into()),
                    };
                    let t0 = std::time::Instant::now();
                    // Published before the syscall, so a sleep that never returns is still
                    // describable. Order matters: timestamp and request first, then the entry
                    // count, so a reader that sees enter>exit always has a valid T0 to subtract.
                    POLL_SLEEP_REQ_MS.store(
                        timeout.map(|t| t.as_millis() as u64).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    POLL_SLEEP_T0_MS.store(mono_ms(), Ordering::Relaxed);
                    POLL_SLEEP_ENTER.fetch_add(1, Ordering::Relaxed);
                    let _ = sys_thread_sync(&mut waiters, timeout);
                    POLL_SLEEP_EXIT.fetch_add(1, Ordering::Relaxed);
                    let slept = t0.elapsed();
                    POLL_MAX_SLEEP_MS.fetch_max(slept.as_millis() as u64, Ordering::Relaxed);
                    // Only when smoltcp named a deadline. An unbounded sleep ending on a word is
                    // now the design, so it is not an overshoot and must not be counted as one.
                    if let Some(timeout) = timeout {
                        if slept > timeout * 4 + std::time::Duration::from_millis(10) {
                            POLL_OVERSLEPT.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        });

        // Stall watchdog. Reports from a *different* thread than the one it describes, which is
        // the whole point: `pollprobe` is called from inside `Core::poll`, so every `site=poll`
        // line reports `phase=3` tautologically and cannot say where the poll thread is. This
        // reads the same word from outside.
        //
        // The predicate used to be "POLL_ITERS did not advance in 2s", justified by the poll
        // loop sleeping at most `FALLBACK` (50ms) per pass and so advancing at >=20/s
        // unconditionally. `POLL_FALLBACK_MS` is now `None` -- the loop sleeps on the wait set
        // and on smoltcp's own deadline, which for an idle compartment is neither -- so a quiet
        // engine legitimately sits still for far longer than 2s and that predicate fires
        // constantly. Measured: 450 STALL lines in an 8-round sweep where every round passed
        // 57/57 with no network failure at all. A detector that cries wolf 450 times cannot
        // report the one real stall, so removing the fallback without fixing this would have
        // silently retired the watchdog rather than merely making it noisy.
        //
        // The replacement does not depend on how long a sleep is *allowed* to be, only on
        // whether the one in flight has outlived what it asked for: `ENTER > EXIT` means a sleep
        // is in progress, and `slpage` is its age against `slpreq`. A sleep that has run past
        // several times its own requested timeout is stuck whatever the fallback constant says.
        // An engine sleeping on an untimed wait set (`slpreq == u64::MAX`) is idle by design and
        // is not a stall; that case is now what the rx-pending check below is for.
        //
        // The heartbeat is the positive control: without it "never stalled" and "watchdog thread
        // never ran" are the same silence, which is the trap the phase reading above already set
        // once today.
        // Diagnostic scaffolding from the UDP-loss investigation, off by default.
        //
        // This thread wakes every 2s for the life of the process, in *every* compartment with
        // a network engine -- 18 of them in a bench boot. That is a permanent periodic wakeup
        // on an otherwise idle machine, paid to watch for a stall whose cause is now known:
        // `wait_ready` read an already-readable fd as a failed registration (net_test_peer).
        // Nothing in the runtime depends on it.
        //
        // Turn it on to investigate a suspected poll-thread stall. Note its predicate needs
        // `POLL_FALLBACK_MS` to be `Some` to mean anything: with the fallback `None` an idle
        // engine legitimately sits still well past 2s, which is what made it emit 450 STALL
        // lines in an 8-round sweep where every round passed.
        if STALL_WATCHDOG {
        std::thread::spawn(|| {
            let mut last = u64::MAX;
            let mut ticks: u64 = 0;
            loop {
                WD_SLEEP_ENTER.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_secs(2));
                WD_SLEEP_EXIT.fetch_add(1, Ordering::Relaxed);
                ticks += 1;
                WATCHDOG_TICKS.fetch_add(1, Ordering::Relaxed);
                let iters = POLL_ITERS.load(Ordering::Relaxed);
                // Stalled = not advancing AND demonstrably owing work: either a timed sleep has
                // overrun its own request, or frames are queued that nothing is collecting.
                let overdue = sleep_inflight_ms().is_some_and(|age| {
                    let req = POLL_SLEEP_REQ_MS.load(Ordering::Relaxed);
                    req != u64::MAX && age > req.saturating_mul(4).max(2000)
                });
                let rx_waiting = matches!(EXT_CORE.load(Ordering::Relaxed), 1);
                if iters == last && (overdue || rx_waiting) {
                    pollprobe("STALL");
                } else if ticks == 1 || ticks % 30 == 0 {
                    // Tick 1, not just every 30th. A peer compartment lives ~20s and the 60s
                    // heartbeat never fired inside one, so the control covered only the two
                    // long-lived compartments and said nothing about the ones under test --
                    // "no stall" and "watchdog never ran here" were the same silence in exactly
                    // the population this exists to describe. One line per compartment at ~2s
                    // arms it per path, inside the window.
                    pollprobe("alive");
                }
                last = iters;
            }
        });
        }

        Self {
            core,
            waiter,
            notify,
            _polling_thread: thread,
            nc_handle,
        }
    }

    pub fn allocate_port(&self, port: Option<u16>) -> Option<u16> {
        let r = net_alloc_port(self.nc_handle, port);
        r.ok()
    }

    pub fn return_port(&self, port: u16) {
        let _ = net_release_port(self.nc_handle, port);
    }

    pub fn get_ephemeral_port(&self) -> Option<u16> {
        self.allocate_port(None)
    }

    pub(super) fn wake(&self) {
        self.notify.store(1, Ordering::SeqCst);
        sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&*self.notify),
                usize::MAX,
            ))],
            None,
        )
        .unwrap();
    }

    pub fn add_socket(&self, socket: Socket<'static>, group: Option<u64>) -> SocketHandle {
        self.core.lock().unwrap().add_socket(socket, group)
    }

    pub fn add_udp_socket(&self, socket: SmolUdpSocket<'static>) -> SocketHandle {
        self.core.lock().unwrap().add_udp_socket(socket)
    }

    // Block until f returns Ok(R), and then return R. Note that f may be called multiple times,
    // and it may be called spuriously. If f returns Err(e) with e.kind() anything other than
    // NonBlock, return the error.
    pub fn blocking<R>(
        &self,
        non_block: bool,
        f: impl FnMut(&mut Core) -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        self.blocking_inner(non_block, false, f)
    }

    /// `blocking`, for operations that queue egress.
    ///
    /// The fast path returns before `core.poll()` and before `self.wake()`, so a `send_slice`
    /// that succeeds immediately queues a datagram and tells nobody. The poll thread is asleep on
    /// `notify` with no timeout -- `Interface::poll_delay` yields `None`, and it computed that
    /// deadline before the send existed -- so the datagram waits for an unrelated wakeup. Whether
    /// it leaves at all then depends on incidental traffic landing inside the peer's timeout,
    /// which is what made the flake intermittent rather than total.
    ///
    /// Measured before this change: the UDP peer's engine ran `calls == fastok == 16`,
    /// `wakes == 0` in every round, pass and fail alike -- it never once took the slow path, so
    /// it never polled and never woke anything. Only `polls` differed (1 when it failed, 3-4 when
    /// it passed), i.e. delivery was decided entirely by how often the background poll thread
    /// happened to run for reasons of its own.
    ///
    /// Kept separate from `blocking` rather than made unconditional: `blocking` is generic over
    /// its closure and cannot tell a send from a read, and an unconditional wake would put a
    /// syscall on every socket operation. Here the closure returns Ok only when `send_slice`
    /// succeeded, so the wake is conditioned on egress actually having been queued.
    pub fn blocking_egress<R>(
        &self,
        non_block: bool,
        f: impl FnMut(&mut Core) -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        self.blocking_inner(non_block, true, f)
    }

    fn blocking_inner<R>(
        &self,
        non_block: bool,
        egress: bool,
        mut f: impl FnMut(&mut Core) -> std::io::Result<R>,
    ) -> std::io::Result<R> {
        // Before the lock, not after: a compartment wedged on `core` or asleep in `wait` still
        // proves itself armed and alive, which is what makes its later zeros readable.
        if (ENGINE_CALLS.fetch_add(1, Ordering::Relaxed) + 1).is_power_of_two() {
            pollprobe("call");
        }
        let mut core = self.core.lock().unwrap();
        if let Ok(r) = f(&mut *core) {
            if (ENGINE_FAST_OK.fetch_add(1, Ordering::Relaxed) + 1).is_power_of_two() {
                // Flag inside the body, never in the condition: `&&` would short-circuit the
                // fetch_add away and the counter would read 0 in the off arm -- indistinguishable
                // from never reaching here.
                if PROBE_UNDER_LOCK {
                    pollprobe("fast");
                }
            }
            if egress {
                // Guard first: wake() is a syscall, and holding the core mutex across it would
                // serialise every other socket operation behind the poll thread waking up.
                drop(core);
                ENGINE_TXWAKE.fetch_add(1, Ordering::Relaxed);
                self.wake();
            }
            return Ok(r);
        }
        // Immediately poll, since we wait to have as up-to-date state as possible.
        core.poll(&self.waiter);
        // **Not for non-blocking callers, and not while holding `core`.**
        //
        // The fast path above drops the guard before `wake()` and explains why: it is a syscall,
        // and holding the core mutex across it serialises every other socket operation --
        // including the poll thread this is trying to wake -- behind it. That rule was not
        // applied here, and this is the hot path for non-blocking I/O.
        //
        // Measured: in a healthy round this path is entered ~10 times. In every wedged round it
        // is entered **80-90 million** times, because `net_read_within`/`net_write_within` retry
        // in a tight `yield_now` loop and each retry cost a full interface poll plus a wake
        // syscall taken under the lock. The poll thread cannot get `core` often enough to
        // transmit, so no data arrives, so the caller retries harder -- self-reinforcing, and it
        // turns a transient stall into a permanent one.
        //
        // For a non-blocking caller the wake is also simply redundant: we just polled inline, so
        // asking the poll thread to wake and poll again immediately buys nothing.
        //
        // This is the amplifier, not necessarily the initiator: something stops data flowing
        // first. Removing it makes a stall recoverable rather than terminal.
        if !non_block {
            drop(core);
            self.wake();
            core = self.core.lock().unwrap();
        }
        loop {
            match f(&mut *core) {
                Ok(r) => {
                    // We have done work, so again, notify the polling thread.
                    self.wake();
                    drop(core);
                    return Ok(r);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if non_block {
                        ENGINE_NB_SLOW.fetch_add(1, Ordering::Relaxed);
                        return Err(e);
                    }
                    self.wake();
                    core = self.waiter.wait(core).unwrap();
                    if (ENGINE_WAKES.fetch_add(1, Ordering::Relaxed) + 1).is_power_of_two() {
                        // Flag inside the body, never in the condition: `&&` would short-circuit
                        // the fetch_add away and the counter would read 0
                        // in the off arm -- indistinguishable from never
                        // reaching here.
                        if PROBE_UNDER_LOCK {
                            pollprobe("wake");
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn track(&self, handle: SocketHandle, port: u16, is_ephem: bool, kind: SockKind) {
        let port = if is_ephem { port } else { 0 };
        self.core
            .lock()
            .unwrap()
            .tracking
            .push((handle, port, kind));
        TRACKING_LEN.fetch_add(1, Ordering::Relaxed);
    }

    pub fn with_iface_for<R>(
        &self,
        addr: SocketAddr,
        f: impl FnOnce(&mut Interface) -> R,
    ) -> Option<R> {
        self.core.lock().unwrap().find_iface_for(addr).map(|i| f(i))
    }
}

impl Core {
    fn new(ifaceset: Vec<IfaceSet>) -> Self {
        let socketset = SocketSet::new(Vec::new());
        Self {
            socketset,
            ifaceset,
            tracking: Vec::new(),
            agg_scratch: HashMap::new(),
            groups: HashMap::new(),
            live: HashSet::new(),
        }
    }

    pub fn add_dns_socket(&mut self, sock: DnsSocket<'static>) -> SocketHandle {
        let handle = self.socketset.add(sock);
        self.live.insert(handle);
        handle
    }

    pub fn add_udp_socket(&mut self, sock: SmolUdpSocket<'static>) -> SocketHandle {
        let handle = self.socketset.add(sock);
        self.live.insert(handle);
        WAITERS.init_waiter(WaitKey::Sock(handle));
        handle
    }

    /// Add a socket, optionally as a member of listener group `group` (see WaitKey).
    pub fn add_socket(&mut self, sock: Socket<'static>, group: Option<u64>) -> SocketHandle {
        let handle = self.socketset.add(sock);
        self.live.insert(handle);
        match group {
            // The group's words belong to the group, not to any one member: resetting them here
            // would drop a sibling's pending connection on the floor.
            Some(group) => {
                self.groups.insert(handle, group);
            }
            None => WAITERS.init_waiter(WaitKey::Sock(handle)),
        }
        handle
    }

    fn wait_key(&self, handle: SocketHandle) -> WaitKey {
        match self.groups.get(&handle) {
            Some(&group) => WaitKey::Group(group),
            None => WaitKey::Sock(handle),
        }
    }

    /// Take `handle` out of its listener group and give it a readiness identity of its own -- what
    /// `accept()` does to the socket it hands back as a stream.
    pub fn detach_from_group(&mut self, handle: SocketHandle) {
        if self.groups.remove(&handle).is_some() {
            WAITERS.init_waiter(WaitKey::Sock(handle));
        }
    }

    /// Queue `handle` for release once it closes, taking it out of any listener group first: a dead
    /// socket left in a group would pin the group readable forever (listener_socket_ready is true
    /// for a closed socket), which spins every poller watching it.
    pub fn retire_socket(&mut self, handle: SocketHandle) {
        if let Some(group) = self.groups.remove(&handle) {
            self.refresh_group(group);
        }
        self.tracking.push((handle, 0, SockKind::Tcp));
        TRACKING_LEN.fetch_add(1, Ordering::Relaxed);
    }

    /// TCP only, despite the name: `Socket` here is `tcp::Socket`, not the enum (see the `use`
    /// at the top of this file).
    ///
    /// # Panics
    /// If `handle` refers to a UDP or DNS socket, or to a socket already removed from the set.
    pub fn get_mutable_socket(&mut self, handle: SocketHandle) -> &mut Socket<'static> {
        self.socketset.get_mut(handle)
    }

    /// Every socket in the set, for identity checks: smoltcp's traces name sockets by endpoint,
    /// which cannot distinguish two sockets bound to the same address.
    pub fn socket_iter(
        &self,
    ) -> impl Iterator<Item = (SocketHandle, &smoltcp::socket::Socket<'static>)> {
        self.socketset.iter()
    }

    pub fn get_mutable_udp_socket(&mut self, handle: SocketHandle) -> &mut SmolUdpSocket<'static> {
        self.socketset.get_mut(handle)
    }

    pub fn get_mutable_dns_socket(&mut self, handle: SocketHandle) -> &mut DnsSocket<'static> {
        self.socketset.get_mut(handle)
    }

    pub fn release_socket(&mut self, handle: SocketHandle) {
        let group = self.groups.remove(&handle);
        self.live.remove(&handle);
        self.socketset.remove(handle);
        match group {
            Some(group) => {
                // The group outlives its members, so recompute rather than forcing it ready --
                // unless this was the last one, in which case the identity itself is done.
                self.refresh_group(group);
                if !self.groups.values().any(|g| *g == group) {
                    WAITERS.free_group(group);
                }
            }
            // Closing must wake both a blocked reader and a blocked writer, so mark both sides
            // ready: mark_waiter only wakes a side it transitions to ready.
            None => WAITERS.mark_waiter(WaitKey::Sock(handle), true, true),
        }
    }

    /// Readiness of one socket, as its wait key's contributor. A listening socket's is "accept()
    /// has work here" rather than "there are bytes to read"; see listener_socket_ready.
    // `sock` is the enum, spelled out because a bare `Socket` in this file is `tcp::Socket`.
    fn socket_readiness(
        &self,
        handle: SocketHandle,
        sock: &smoltcp::socket::Socket<'static>,
    ) -> (bool, bool) {
        match sock {
            // rx_shutdown is per-fd and invisible here, so it is passed as false: this publishes
            // the socket's own readiness. A locally read-shutdown fd is still reported ready by
            // can_read, which does see the flag, and poll ORs the two.
            smoltcp::socket::Socket::Udp(socket) => {
                (udp_socket_ready(socket, false), socket.can_send())
            }
            smoltcp::socket::Socket::Tcp(socket) => {
                if self.groups.contains_key(&handle) {
                    // Write is meaningless for a listener; SmolTcpListener::can_write agrees.
                    (listener_socket_ready(socket), false)
                } else {
                    (stream_socket_ready(socket, false), socket.can_send())
                }
            }
            _ => (false, false),
        }
    }

    /// Re-publish one readiness source's state into its wait words, using the same expressions
    /// poll() uses. Call this after an app-side operation changes readiness -- a read that drains
    /// the receive buffer, a write that fills the send buffer, an accept that takes a connection
    /// out of a listener group. Without it the words only move on the background poll pass, so a
    /// drain followed by a refill before the next pass is never observed as a not-ready period at
    /// all, which is fatal for any edge-triggered consumer (see the EV_CLEAR handling in
    /// runtime::file::kqueue).
    pub fn refresh_waiter(&self, handle: SocketHandle) {
        self.refresh_key(self.wait_key(handle));
    }

    /// Per-state census of every listener group, sampled on a schedule the poll loop controls.
    ///
    /// Not in `refresh_key`: that fires from `waitpoint()`, which a peer calls once, so it sampled
    /// only the instants either side of the window under test and nothing within it. `Core::poll`
    /// publishes through `mark_waiter` directly and bypasses `refresh_key` altogether.
    pub fn census_groups(&self) {
        let groups: std::collections::BTreeSet<u64> = self.groups.values().copied().collect();
        for g in groups {
            let mut census = [0u16; 11];
            let mut ready = false;
            for (handle, sock) in self.socketset.iter() {
                if self.wait_key(handle) != WaitKey::Group(g) {
                    continue;
                }
                let (r, _) = self.socket_readiness(handle, sock);
                ready |= r;
                if let smoltcp::socket::Socket::Tcp(t) = sock {
                    census[match t.state() {
                        State::Closed => 0,
                        State::Listen => 1,
                        State::SynSent => 2,
                        State::SynReceived => 3,
                        State::Established => 4,
                        State::FinWait1 => 5,
                        State::FinWait2 => 6,
                        State::CloseWait => 7,
                        State::Closing => 8,
                        State::LastAck => 9,
                        State::TimeWait => 10,
                    }] += 1;
                }
            }
            if !ready {
                groupcensus(
                    "pollthread",
                    GROUP_NOTREADY.fetch_add(1, Ordering::Relaxed) + 1,
                    &census,
                );
            }
        }
    }

    pub fn refresh_group(&self, group: u64) {
        self.refresh_key(WaitKey::Group(group));
    }

    fn refresh_key(&self, key: WaitKey) {
        let (read, write) = match key {
            // A Sock key has exactly one contributor and we know which one, so look it up rather
            // than walking the set. This is on the per-read and per-write path -- every `read()`
            // and `write()` calls refresh_waiter -- where the scan cost one `groups` hash probe
            // per socket in the set, per syscall. (Reaching here with Sock(h) implies h is not in
            // a group: refresh_waiter routes through wait_key, which yields Group for a member.)
            WaitKey::Sock(handle) => {
                if self.live.contains(&handle) {
                    self.socket_readiness(
                        handle,
                        self.socketset
                            .get::<smoltcp::socket::Socket<'static>>(handle),
                    )
                } else {
                    (false, false)
                }
            }
            // A group aggregates every member, so it genuinely needs the walk.
            WaitKey::Group(_) => {
                let (mut read, mut write) = (false, false);
                // Per-state census of the backlog, for the not-ready case below.
                let mut census = [0u16; 11];
                for (handle, sock) in self.socketset.iter() {
                    if self.wait_key(handle) != key {
                        continue;
                    }
                    let (r, w) = self.socket_readiness(handle, sock);
                    read |= r;
                    write |= w;
                    if let smoltcp::socket::Socket::Tcp(t) = sock {
                        census[match t.state() {
                            State::Closed => 0,
                            State::Listen => 1,
                            State::SynSent => 2,
                            State::SynReceived => 3,
                            State::Established => 4,
                            State::FinWait1 => 5,
                            State::FinWait2 => 6,
                            State::CloseWait => 7,
                            State::Closing => 8,
                            State::LastAck => 9,
                            State::TimeWait => 10,
                        }] += 1;
                    }
                }
                // A listener that is not ready is the whole question: `listener_socket_ready`
                // excludes SYN-RECEIVED deliberately, and nothing ever reaps a socket stuck
                // there, so a backlog of them goes silently deaf. This says whether that is
                // what is happening rather than leaving it as a reading of the source.
                if !read {
                    let n = GROUP_NOTREADY.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_power_of_two() {
                        groupcensus("readiness", n, &census);
                    }
                }
                (read, write)
            }
        };
        WAITERS.mark_waiter(key, read, write);
    }

    fn poll(&mut self, waiter: &Condvar) -> bool {
        if (ENGINE_POLLS.fetch_add(1, Ordering::Relaxed) + 1).is_power_of_two() {
            // Flag inside the body, never in the condition: `&&` would short-circuit the
            // fetch_add away and the counter would read 0 in the off arm -- indistinguishable
            // from never reaching here.
            if PROBE_UNDER_LOCK {
                pollprobe("poll");
            }
        }
        POLL_SUB.store(10, Ordering::Relaxed);
        let mut res = false;
        for ifaceset in &mut self.ifaceset {
            res |= ifaceset.poll(&mut self.socketset);
        }
        POLL_SUB.store(15, Ordering::Relaxed);
        if res {
            // Aggregate before publishing: several listening sockets share one WaitKey, and marking
            // them one at a time would let a not-ready sibling clear a ready one's word (and count
            // a bogus falling edge while doing it).
            POLL_SUB.store(16, Ordering::Relaxed);
            let mut agg = std::mem::take(&mut self.agg_scratch);
            agg.clear();
            for (handle, sock) in self.socketset.iter() {
                let (read, write) = self.socket_readiness(handle, sock);
                let entry = agg.entry(self.wait_key(handle)).or_insert((false, false));
                entry.0 |= read;
                entry.1 |= write;
            }
            POLL_SUB.store(17, Ordering::Relaxed);
            for (key, (read, write)) in agg.drain() {
                WAITERS.mark_waiter(key, read, write);
            }
            self.agg_scratch = agg;
            POLL_SUB.store(18, Ordering::Relaxed);
            // Notify the CV so that other waiting threads can retry their blocking operations.
            waiter.notify_all();
            POLL_SUB.store(19, Ordering::Relaxed);
        }
        // NOTE: notifying *unconditionally* here livelocks, and briefly did.
        //
        // The bug being fixed was real: `res` is smoltcp's `SocketStateChanged`, which cannot see
        // `check_completions()` reclaiming tx packets, so a sender blocked for want of a slot
        // slept through the event it was waiting for. But notifying on every poll instead closes a
        // cycle -- `blocking_inner`'s WouldBlock arm calls `wake()` before it waits, so the woken
        // sender's failed retry wakes the poll thread, which polls, which notifies, forever.
        // Progress is reported through `IfaceSet::poll` (see `Pair::progress`) instead, so the
        // notify stays conditional and only fires when something a waiter cares about happened.
        res
    }

    fn poll_time(&mut self) -> Option<Duration> {
        let mut min_time = None;
        for ifaceset in &mut self.ifaceset {
            if let Some(time) = ifaceset.poll_time(&mut self.socketset) {
                min_time = Some(min_time.map_or(time, |t: Duration| t.min(time)));
            }
        }
        min_time
    }

    fn find_iface_for(&mut self, addr: SocketAddr) -> Option<&mut Interface> {
        for ifaceset in &mut self.ifaceset {
            if let Some(iface) = ifaceset.find_iface_for(addr) {
                return Some(iface);
            }
        }
        None
    }

    pub fn iface_for_dns(&mut self) -> Option<&mut Interface> {
        for ifaceset in &mut self.ifaceset {
            if let Some(iface) = ifaceset.find_iface_for_dns() {
                return Some(iface);
            }
        }
        None
    }
}

fn get_twznet_device_and_interface() -> (Interface, NetClient) {
    let mut device = NetClient::open(NetClientConfig {}).unwrap();

    // Create interface
    let mut config = Config::new(device.info.hwaddr.into());
    config.random_seed = std::random::random(..);

    // Static-address override. net-srv hands out addresses in the order compartments happen to
    // open a client, which is fine for talking to the outside world but leaves two compartments
    // unable to name each other ahead of time -- so a test that needs a client and a server in
    // separate compartments can pin both. The MAC still comes from net-srv, and net-srv's
    // on-host delivery is keyed on MAC, so overriding the address here needs no cooperation from
    // it (and slirp NATs whatever source address it sees).
    let addr = match std::env::var("TWZ_NET_ADDR")
        .ok()
        .and_then(|a| a.parse::<std::net::IpAddr>().ok())
    {
        Some(addr) => {
            tracing::info!("using static address {} from TWZ_NET_ADDR", addr);
            addr
        }
        None => device.info.addr,
    };

    // Take the diagnostic octet from the *effective* address, not `device.info.addr`.
    // net-srv hands out 10.0.2.15..30 in whatever order compartments open a client; the tests pin
    // themselves with TWZ_NET_ADDR (.100 parent, .101-.111 peers). Reading net-srv's assignment
    // gave octets 16-30 and no `.106` at all -- an identifier that looks authoritative while
    // quantifying over the wrong set, which is exactly what this file warns about for the
    // "new net client: addr = ..." log line.
    if let std::net::IpAddr::V4(v4) = addr {
        ENGINE_OCTET.store(v4.octets()[3] as u64, Ordering::Relaxed);
    }
    // Outside the `if`, deliberately. This is the positive control -- every compartment that
    // builds an engine announces itself with all counters at their initial values, so silence
    // carries one meaning (no engine was built) rather than standing in for any counter that
    // failed to cross a threshold. Written first *inside* the V4 arm under a comment calling it
    // unconditional, which would have made a non-V4 compartment read as unarmed: the control
    // reacquiring the exact blind spot it exists to remove, behind a comment asserting otherwise.
    // A non-V4 address now reports octet=999, which is a value, not an absence.
    //
    // Force `ENGINE_T0` to initialise *here*, at engine startup, and not wherever it is first
    // dereferenced. It is a `lazy_static`, so its zero point is set by the first reader -- which
    // would otherwise be `mono_ms()` inside the poll thread's sleep bracket. `engms` would then
    // measure "time since the first sleep" while being read as "engine age", and would report a
    // small number no matter which of the two hypotheses were true: an instrument that cannot
    // distinguish the cases it exists to distinguish.
    let _ = *ENGINE_T0;
    pollprobe("init");

    tracing::info!(
        "setting up interface with addr {} and prefix {}",
        addr,
        device.info.addr_prefix_len
    );
    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(
                IpAddress::from(addr),
                device.info.addr_prefix_len,
            ))
            .unwrap();
    });
    match device.info.gateway {
        std::net::IpAddr::V4(ipv4_addr) => iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::from(ipv4_addr))
            .unwrap(),
        std::net::IpAddr::V6(ipv6_addr) => iface
            .routes_mut()
            .add_default_ipv6_route(Ipv6Address::from(ipv6_addr))
            .unwrap(),
    };

    (iface, device)
}
