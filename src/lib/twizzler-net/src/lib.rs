use secgate::TwzError;
pub use twizzler_io::packet;

mod client;
pub mod drivers;
mod endpoint;
mod server;

pub use client::{
    NetClient, NetClientConfig, NetClientOpenInfo, NetClientRxToken, NetClientTxToken,
    net_alloc_port, net_release_port,
};
pub use server::{NetServer, NetServerRxToken, NetServerTxToken};
// Re-exported for the engine's POLLPROBE line: this compartment's own copy of the ring-wake
// counters (statics are per-compartment). `RING_NO_WAITER` climbing during a stall while the
// consumer's kernel row says it is parked armed is the wake-skip arm of the missed-wake split.
pub use twizzler_queue::{RING_NO_WAITER, RING_WOKE};

pub type PacketNum = u32;

/// Arm selector: use the non-blocking queue paths on the poll thread.
///
/// `false` restores the blocking `SubmissionFlags::empty()` submits that ran inside `Core::poll`
/// while holding the engine core mutex. Kept as a flippable constant so the fix has a control on
/// the same toolchain, and greppable in the source so an arm cannot be misattributed.
pub const NONBLOCK_POLL_QUEUE: bool = true;

/// Frames dropped, and completions deferred, because a ring was full. Never silent: a blocking
/// submit that used to wedge the compartment becomes a drop, and a drop nobody counts is just a
/// quieter bug.
pub static POLLQ_TX_DROPPED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static POLLQ_COMP_DEFERRED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Frames whose *submission* to the client's rx ring returned Ok, counted at the submission
/// itself rather than upstream of it.
///
/// `inject` returns the number of frames it copied into packet slots, and `note_inject_ok` counts
/// that -- but the handoff is `submit_rx`, which runs afterwards and can drop the whole batch. So
/// the per-address "local dst .N reached M" figures count frames that reached a *slot*, not
/// frames that reached the queue, and every reading built on them inherits that. This counts the
/// operation that can fail, at the place it can fail.
/// Datagrams `UdpSocket::write_to` handed to smoltcp (i.e. `send_slice` returned Ok).
///
/// Paired with `DEV_TX_FRAMES`: smoltcp accepting a datagram into the socket's tx buffer is not
/// the same as it dispatching one. If this climbs and `DEV_TX_FRAMES` does not, the datagram died
/// between the socket and the device -- dispatch dropped it (unresolved neighbour, no route),
/// which the caller cannot see because `send_slice` already returned Ok.
pub static UDP_SEND_ACCEPTED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Ethernet frames actually handed to the device by smoltcp's `TxToken::consume`.
pub static DEV_TX_FRAMES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub static POLLQ_TX_SUBMITTED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Whether the diagnostic class `class` was requested via `TWZ_DIAG` (comma-separated list, or
/// `all`). Read once per compartment; init forwards the boot-line `--diag=<classes>` into the
/// environment, and init logs the resulting value at boot so a silent log provably means "off",
/// not "instrument broke".
pub fn diag_enabled(class: &str) -> bool {
    static SET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let set = SET.get_or_init(|| std::env::var("TWZ_DIAG").unwrap_or_default());
    set.split(',').any(|c| c == class || c == "all")
}

/// Print the queue-handoff totals.
///
/// Not milestone-gated like [`note_pollq`]: the question these answer is whether a drop happened
/// *at all*, and a counter that only prints when it is nonzero cannot distinguish "no drops" from
/// "never reached". `POLLQ ... reached N` has never appeared in ~65,000 sweep logs, which is
/// exactly that ambiguity. The silence-ambiguity role moved to init's one `TWZDIAG` boot line:
/// with `net` listed there, no POLLQSTAT means the caller never ran, and without it, off.
pub fn report_pollq() {
    if !diag_enabled("net") {
        return;
    }
    twizzler_abi::klog_println!(
        "POLLQSTAT submitted={} tx_dropped={} comp_deferred={}",
        POLLQ_TX_SUBMITTED.load(core::sync::atomic::Ordering::Relaxed),
        POLLQ_TX_DROPPED.load(core::sync::atomic::Ordering::Relaxed),
        POLLQ_COMP_DEFERRED.load(core::sync::atomic::Ordering::Relaxed),
    );
}

/// Report at power-of-two milestones only; this is on the per-frame path.
pub fn note_pollq(counter: &core::sync::atomic::AtomicU64, what: &str) {
    let n = counter.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    if n.is_power_of_two() {
        twizzler_abi::klog_println!("POLLQ {} reached {}", what, n);
    }
}

pub const MAX_PACKETS_SET: usize = 8;
pub const INVALID_PACKET: PacketNum = !0;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PacketSet([u32; MAX_PACKETS_SET]);

impl PacketSet {
    pub fn new() -> Self {
        Self([INVALID_PACKET; _])
    }

    pub fn from_slice(slice: &[u32]) -> (Self, usize) {
        let mut arr = [INVALID_PACKET; _];
        let len = MAX_PACKETS_SET.min(slice.len());
        arr[0..len].copy_from_slice(&slice[0..len]);
        (Self(arr), len)
    }

    pub fn push(&mut self, num: PacketNum) -> Option<()> {
        let inv = self.0.iter().position(|p| *p == INVALID_PACKET)?;
        self.0[inv] = num;
        Some(())
    }
}

pub struct PacketSetIter<'a> {
    set: &'a PacketSet,
    index: usize,
}

impl<'a> Iterator for PacketSetIter<'a> {
    type Item = PacketNum;

    fn next(&mut self) -> Option<Self::Item> {
        let mut num = INVALID_PACKET;
        while num == INVALID_PACKET && self.index < MAX_PACKETS_SET {
            num = self.set.0[self.index];
            self.index += 1;
        }
        if num == INVALID_PACKET {
            None
        } else {
            Some(num)
        }
    }
}

impl<'a> IntoIterator for &'a PacketSet {
    type Item = PacketNum;
    type IntoIter = PacketSetIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        PacketSetIter {
            set: self,
            index: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ServerMsg {
    kind: ServerMsgKind,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum ClientMsgKind {
    Tx(PacketSet),
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum ServerMsgKind {
    Tx(PacketSet),
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ClientMsg {
    kind: ClientMsgKind,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ClientRet {}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ServerRet {}

#[secgate::gatecall]
pub fn start_network() -> Result<(), TwzError> {}

#[secgate::gatecall]
fn twz_net_drop_client(handle: secgate::util::Descriptor) -> Result<(), TwzError> {}

#[secgate::gatecall]
fn twz_net_open_client(config: NetClientConfig) -> Result<NetClientOpenInfo, TwzError> {}

#[secgate::gatecall]
fn twz_net_alloc_port(
    handle: secgate::util::Descriptor,
    port: Option<u16>,
) -> Result<u16, TwzError> {
}

#[secgate::gatecall]
fn twz_net_release_port(handle: secgate::util::Descriptor, port: u16) -> Result<(), TwzError> {}
