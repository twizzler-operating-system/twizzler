use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
};

use smoltcp::{
    phy::{Device, RxToken},
    time::Instant,
    wire::{
        ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet,
        Ipv6Packet, PrettyPrinter, TcpPacket,
    },
};
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync};
use twizzler_net::{MAX_PACKETS_SET, NetServer};
use virtio_net::TxBuffer;

use crate::{addr::ClientAddr, NETINFO};

pub struct Client {
    pub ep: Mutex<NetServer>,
    jh: OnceLock<JoinHandle<()>>,
    pub active: AtomicBool,
    pub ports: Mutex<HashMap<u16, usize>>,
    pub addr: ClientAddr,
}

impl Client {
    pub fn new(ep: NetServer, addr: ClientAddr) -> Arc<Self> {
        let client = Arc::new(Client {
            ep: Mutex::new(ep),
            jh: OnceLock::new(),
            active: AtomicBool::new(true),
            ports: Mutex::new(HashMap::new()),
            addr,
        });
        let _client = client.clone();
        let jh = std::thread::spawn(move || client_thread(_client));
        client.jh.set(jh).unwrap();
        client
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

/// Where a frame leaving a client should go.
#[derive(Clone, Copy, Debug)]
enum Dest {
    /// Off-box, out the NIC -- the ordinary case.
    Device,
    /// Another client on this host, addressed by its MAC.
    Local(EthernetAddress),
    /// Broadcast/multicast: every other client *and* the NIC.
    Flood,
}

/// The interface MTU net-srv advertises to every client (`NetServer::capabilities`).
const MTU: usize = 1514;

/// True length of the Ethernet frame sitting in `buf`, which is a whole 2048-byte packet slot.
///
/// The packet protocol carries no length -- `RxToken::consume` hands back the entire slot -- so
/// the only surviving record of how much of it is real is the headers. Every consumer already
/// relies on exactly that: smoltcp reads the ethertype, then the IP total-length, and ignores the
/// tail. This computes the same number those parsers do, so trimming to it cannot change what any
/// receiver sees; it only stops us copying, and transmitting, the ~2KB of stale slot behind it.
///
/// Anything unrecognised falls back to the full slot, i.e. to today's behaviour. The fallback is
/// the safe direction: too long is what we already do everywhere.
fn frame_len(buf: &[u8]) -> usize {
    let Ok(frame) = EthernetFrame::new_checked(buf) else {
        return buf.len();
    };
    let payload = match frame.ethertype() {
        EthernetProtocol::Ipv4 => Ipv4Packet::new_checked(frame.payload())
            .ok()
            .map(|p| p.total_len() as usize),
        EthernetProtocol::Ipv6 => Ipv6Packet::new_checked(frame.payload())
            .ok()
            .map(|p| p.total_len()),
        // ArpPacket has no buffer_len (that lives on Repr), but the wire layout is fixed:
        // 8 bytes of header, then sender/target hardware and protocol addresses twice over.
        EthernetProtocol::Arp => ArpPacket::new_checked(frame.payload())
            .ok()
            .map(|p| 8 + 2 * (p.hardware_len() as usize + p.protocol_len() as usize)),
        _ => None,
    };
    match payload {
        Some(n) => (EthernetFrame::<&[u8]>::header_len() + n).min(buf.len()),
        None => buf.len(),
    }
}

/// Frames handed to the NIC, and how many exceeded the MTU we advertise.
///
/// These exist because the test suite cannot go red on the defect they measure. net-srv hands the
/// NIC the whole packet slot rather than the frame; QEMU's SLIRP backend parses by header and
/// ignores the tail, so an oversized frame is still delivered and every test passes. Reading test
/// outcomes would therefore show green through both the bug and its fix, and a change that merely
/// shrank the copies would be indistinguishable from one that corrected the framing. These
/// counters separate the two: DEV_TX_OVERSIZED is ~100% of frames before the fix and must be 0
/// after, and the byte totals say how much of what we copied was slot padding.
static DEV_TX_FRAMES: AtomicU64 = AtomicU64::new(0);
static DEV_TX_OVERSIZED: AtomicU64 = AtomicU64::new(0);
static DEV_TX_MAXLEN: AtomicU64 = AtomicU64::new(0);
static DEV_TX_SENTBYTES: AtomicU64 = AtomicU64::new(0);
static DEV_TX_FRAMEBYTES: AtomicU64 = AtomicU64::new(0);
static DEV_TX_REPORTED: AtomicU64 = AtomicU64::new(0);

/// Which length net-srv hands the NIC and copies on the local path.
///
/// `false` reproduces the pre-fix behaviour exactly (the whole packet slot) and is the control
/// arm; `true` is the fix. A named const rather than an edit to the call sites so that which arm
/// a given build actually was can be read straight out of the source with one grep, instead of
/// being inferred from a file mtime -- a flag flipped before a build window is invisible to any
/// `find -newermt` audit.
const TRIM_TX_TO_FRAME: bool = true;

/// Record one frame on its way to the NIC. `sent` is what we actually hand over; `real` is what
/// the headers say the frame is. They are equal only once the fix is in.
fn note_dev_tx(sent: usize, real: usize) {
    DEV_TX_FRAMES.fetch_add(1, Ordering::Relaxed);
    if sent > MTU {
        DEV_TX_OVERSIZED.fetch_add(1, Ordering::Relaxed);
    }
    DEV_TX_MAXLEN.fetch_max(sent as u64, Ordering::Relaxed);
    DEV_TX_SENTBYTES.fetch_add(sent as u64, Ordering::Relaxed);
    DEV_TX_FRAMEBYTES.fetch_add(real as u64, Ordering::Relaxed);
}

fn classify(buf: &[u8], local_macs: &[EthernetAddress]) -> Dest {
    let Ok(frame) = EthernetFrame::new_checked(buf) else {
        return Dest::Device;
    };
    let dst = frame.dst_addr();
    if dst.is_broadcast() || dst.is_multicast() {
        // ARP between two clients depends on this: without flooding, a client could never learn a
        // sibling's MAC and no local destination would ever be reachable.
        Dest::Flood
    } else if local_macs.contains(&dst) {
        Dest::Local(dst)
    } else {
        // Anything else -- the gateway, or a MAC we have no record of -- belongs off-box.
        //
        // Counted here rather than at the call site so the destination is read from the frame that
        // was actually classified. Gateway traffic lands in this arm too, so a non-zero count is
        // not by itself a fault; what makes it diagnostic is *which* MAC it names.
        let n = DEVICE_UNICAST.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            tracing::warn!(
                "unicast frame for {} sent off-box (not in local snapshot); {} so far",
                dst,
                n
            );
        }
        Dest::Device
    }
}

/// Frames dropped by `inject_local` because the target's rx pool had no free packet.
///
/// Global rather than per-client: the warning names the target, and a per-client map would need a
/// lock on a path that already holds two.
static LOCAL_RX_DROPS: AtomicU64 = AtomicU64::new(0);

/// `Dest::Local` frames that reached `inject_local` and matched no live client.
///
/// `classify` decides Local from a `local_macs` snapshot taken before this client's `ep` is held;
/// `inject_local` re-takes the handles lock afterwards, and a sibling that went away in between
/// leaves the frame with nowhere to go. It is not injected, and because the destination is Local
/// it is never handed to the NIC either -- the one delivery outcome on this path that produces no
/// record at all. Counted separately for FINs because a lost FIN is the failure under
/// investigation and a lost ARP retry is not.
static LOCAL_NOMATCH: AtomicU64 = AtomicU64::new(0);
static LOCAL_NOMATCH_FIN: AtomicU64 = AtomicU64::new(0);

/// Frames `inject_local` successfully handed to a sibling.
///
/// The drop counter alone cannot distinguish "no frame was lost here" from "no frame came through
/// here at all", and that ambiguity is load-bearing: 261 rounds read `LOCAL_RX_DROPS == 0` without
/// establishing that local delivery was carrying anything. This is the denominator for that zero.
static LOCAL_RX_OK: AtomicU64 = AtomicU64::new(0);

/// Locally-injected IPv4 frames, bucketed by the last octet of the destination address.
///
/// The whole open question is whether the twenty lost datagrams ever reached net-srv at all, and
/// `LOCAL_RX_OK` cannot say: it is global, so it proves delivery was carrying traffic without
/// attributing any of it. Both ends of this test pin their address (`TWZ_NET_ADDR` 10.0.2.100 and
/// .106), so the last octet identifies them directly and needs no correlation against net-srv's own
/// sequential client numbering -- which names a *different* address and has misled a reader of
/// these logs before.
///
/// Indexed rather than mapped so the hot path is exactly one relaxed `fetch_add`: no lock, no
/// allocation, no logging, no branch on socket state. Reporting happens elsewhere, on a trigger
/// that already exists.
static LOCAL_RX_BY_DST: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];

/// Drops bucketed by destination octet. `LOCAL_RX_DROPS` is global, which cannot answer the
/// question actually under investigation: whether *one* backed-up client is losing everything
/// while the rest are fine. A boot drops a handful of frames for ordinary reasons, so a global
/// count near zero and a total wipe-out of one destination look the same.
static LOCAL_RX_DROPS_BY_DST: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];

/// The same, bucketed by the last octet of the IPv4 *source*.
///
/// This is the higher-contrast half, and the reason is a property of the test rather than of the
/// network: `net_test` gives **every one of its peers a distinct address** (.101 connect-send,
/// .102 rearm, ... .106 udp-send, .107 connect-idle, ...), so source `.106` is traffic from the UDP
/// peer and from nothing else. Destination `.100` is the parent and carries every test's traffic at
/// once, which buries twenty datagrams in hundreds.
///
/// So `.106` answers the open question directly: roughly twenty in a passing round, and near zero
/// in a failing one iff the peer's datagrams never reached net-srv at all.
static LOCAL_RX_BY_SRC: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
/// TCP frames carrying FIN, counted where every client frame passes: `classify`.
///
/// The lost-FIN question, reduced to one measurement. `TcpStreamInner::drop` is now known to run
/// (tcpdrops identical in failing and passing rounds), so `close()` is called and the FIN is
/// queued -- yet only one frame ever reaches the peer, while the parent's engine polls 256+ times.
/// Two possibilities remain and this separates them: **`TX_FIN_BY_SRC[100] == 0` in a failing
/// round means the parent's smoltcp never emitted the FIN** (fault upstream, in the engine);
/// nonzero means it emitted one and net-srv lost or misrouted it (fault here).
///
/// The `_DEVICE`/`_FLOOD` splits catch the specific misroute worth suspecting: a FIN sent out the
/// NIC instead of delivered on-box would never increment the per-destination counter that made
/// this population visible in the first place.
/// FINs by IPv4 *destination*. `TX_FIN_BY_SRC` answers "did the parent emit FINs" and cannot
/// answer "was one of them for the peer" -- which is the question the failing rounds turn on.
static TX_FIN_BY_DST: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
static TX_FIN_BY_SRC: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
static TX_FIN_LOCAL: AtomicU64 = AtomicU64::new(0);
static TX_FIN_DEVICE: AtomicU64 = AtomicU64::new(0);
static TX_FIN_FLOOD: AtomicU64 = AtomicU64::new(0);
static TX_FIN_REPORTED: AtomicU64 = AtomicU64::new(0);

/// Source octet of an IPv4/TCP frame whose FIN flag is set, if it is one.
fn tcp_fin_src_octet(frame: &[u8]) -> Option<(u8, u8)> {
    let eth = EthernetFrame::new_checked(frame).ok()?;
    if eth.ethertype() != EthernetProtocol::Ipv4 {
        return None;
    }
    let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
    if ip.next_header() != IpProtocol::Tcp {
        return None;
    }
    let tcp = TcpPacket::new_checked(ip.payload()).ok()?;
    tcp.fin()
        .then(|| (ip.src_addr().octets()[3], ip.dst_addr().octets()[3]))
}

/// Locally-injected frames that did not yield an IPv4 (src, dst), split by *why*.
///
/// These exist because two successive versions of this counter reported an empty address list, and
/// an empty list has more than one explanation: "no such frame was injected", "the header parse is
/// wrong", and "the frame parsed as IPv4 but the IPv4 parse failed". The first version had no
/// negative bucket at all; the second bucketed ARP and unknown ethertypes but let a failed
/// `Ipv4Packet::new_checked` fall through to `None` silently -- the same mistake one level down.
///
/// So every path out of `ipv4_src_dst_octets` now lands in exactly one bucket, and the reporting
/// line prints `accounted` against the total. **If those two disagree, something is uncounted and
/// no conclusion may be drawn from an absent address** -- the instrument audits itself instead of
/// relying on me to have enumerated the paths correctly, which I twice did not.
static LOCAL_RX_ARP: AtomicU64 = AtomicU64::new(0);
static LOCAL_RX_NOT_ETH: AtomicU64 = AtomicU64::new(0);
static LOCAL_RX_BAD_IPV4: AtomicU64 = AtomicU64::new(0);
static LOCAL_RX_OTHER_ET: AtomicU64 = AtomicU64::new(0);
/// The most recent ethertype that fell into `LOCAL_RX_OTHER_ET`, so "other" names itself.
static LOCAL_RX_LAST_ETHERTYPE: AtomicU64 = AtomicU64::new(0);
/// Frames counted into the src/dst tables.
static LOCAL_RX_IPV4: AtomicU64 = AtomicU64::new(0);

/// Last octets of the IPv4 (source, destination) of `frame`, if it is IPv4.
///
/// Parsed with smoltcp's own accessors rather than hand-written offsets, so this cannot disagree
/// with `classify` above about where the headers are. Every address here is in 10.0.2.0/24, so the
/// last octet identifies the host. Every return path increments exactly one counter.
fn ipv4_src_dst_octets(frame: &[u8]) -> Option<(u8, u8)> {
    let Ok(eth) = EthernetFrame::new_checked(frame) else {
        LOCAL_RX_NOT_ETH.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    match eth.ethertype() {
        EthernetProtocol::Ipv4 => match Ipv4Packet::new_checked(eth.payload()) {
            Ok(ip) => {
                LOCAL_RX_IPV4.fetch_add(1, Ordering::Relaxed);
                Some((ip.src_addr().octets()[3], ip.dst_addr().octets()[3]))
            }
            Err(_) => {
                LOCAL_RX_BAD_IPV4.fetch_add(1, Ordering::Relaxed);
                None
            }
        },
        EthernetProtocol::Arp => {
            LOCAL_RX_ARP.fetch_add(1, Ordering::Relaxed);
            None
        }
        other => {
            LOCAL_RX_OTHER_ET.fetch_add(1, Ordering::Relaxed);
            LOCAL_RX_LAST_ETHERTYPE.store(u16::from(other) as u64, Ordering::Relaxed);
            None
        }
    }
}

/// Unicast frames sent off-box because their destination MAC was absent from the sender's
/// `local_macs` snapshot.
///
/// A sibling that registered after the snapshot is not yet local, so its frame goes out the NIC and
/// is lost -- `client_thread`'s comment says "ARP retries", which is true of ARP and of TCP and
/// false of a UDP datagram. Counted separately from the rx-pool drop because it is a *different*
/// silent loss on the same delivery path, and nothing has ever measured it.
static DEVICE_UNICAST: AtomicU64 = AtomicU64::new(0);

/// Inject `frame` into every client matching `f`, skipping the sender.
///
/// Takes the handles lock and then each target's `ep` lock, the same order `device_thread` uses.
/// The caller must not hold its own `ep`: two client threads cross-injecting while each held its
/// own would deadlock, which is why `client_thread` copies frames out and dispatches them only
/// after dropping that lock.
/// Whether server->client delivery batches frames into one queue message.
///
/// `false` reproduces the pre-change behaviour exactly -- one frame per queue message, the shape
/// the send half had before `NetServer::inject` existed -- by capping the batch at one. Everything
/// else (loop order, lock scope, reporting outside the locks) is identical between arms, so the
/// only thing varying is the batch cap.
///
/// A const rather than two source trees, for the same reason `FILTER_UNICAST` in device.rs is one:
/// both arms build from a single tree state, and which arm a build actually was is one grep on the
/// source rather than an inference from a file mtime, which cannot see a constant flipped before
/// the build window.
const BATCH_LOCAL_DELIVERY: bool = true;

/// Frames per queue message. `MAX_PACKETS_SET` is what `PacketSet` has always carried.
const BATCH_MAX: usize = if BATCH_LOCAL_DELIVERY {
    MAX_PACKETS_SET
} else {
    1
};

/// Engagement counters for multipacket delivery: frames handed to clients, and queue messages
/// used to do it.
///
/// The ratio is the *control* for this change, not a performance figure. `frames/msgs == 1.0`
/// means the batch never filled and the change is inert, in which case any timing difference is
/// something else and must not be attributed here. It can only ever confirm that batching
/// happened; it says nothing about whether batching helped.
static BATCH_FRAMES: AtomicU64 = AtomicU64::new(0);
static BATCH_MSGS: AtomicU64 = AtomicU64::new(0);

/// What an injected frame is, so the singleton population can be *named* rather than assumed.
///
/// The batch ratio alone cannot separate "the cap is too small" from "there was only ever one
/// frame to send". A pure ACK is emitted on its own poll with nothing to accompany it: it is
/// structurally a singleton and no batch cap can help it. Payload-carrying segments are the
/// population a larger cap could coalesce, so they are what a cap change has to be read against.
/// Without this split, a ratio that fails to rise has two explanations and no way to choose.
#[derive(Clone, Copy)]
enum FrameClass {
    TcpData = 0,
    TcpAck = 1,
    Arp = 2,
    Other = 3,
}

fn classify_frame(frame: &[u8]) -> FrameClass {
    let Ok(eth) = EthernetFrame::new_checked(frame) else {
        return FrameClass::Other;
    };
    match eth.ethertype() {
        EthernetProtocol::Arp => FrameClass::Arp,
        EthernetProtocol::Ipv4 => {
            let Ok(ip) = Ipv4Packet::new_checked(eth.payload()) else {
                return FrameClass::Other;
            };
            if ip.next_header() != IpProtocol::Tcp {
                return FrameClass::Other;
            }
            let Ok(tcp) = TcpPacket::new_checked(ip.payload()) else {
                return FrameClass::Other;
            };
            if tcp.payload().is_empty() {
                FrameClass::TcpAck
            } else {
                FrameClass::TcpData
            }
        }
        _ => FrameClass::Other,
    }
}

/// Whether the batch-shape milestone line is printed.
///
/// **Default off, and the default matters.** The counters below are relaxed atomics and free;
/// *printing* them is a serial-console write on a path that already prints one milestone line,
/// and enabling it doubled the delivery path's per-frame console output. The arms that carried it
/// (`mps8`/`mps16`) ran ~60% slower on both throughput benches than the otherwise-identical
/// `nb-on1` at the same HEAD -- enough that a timing number taken from an armed tree and
/// inherited as a baseline would corrupt every later comparison. Ratios are intensive and were
/// unaffected (3.52 armed vs 3.50/3.49/3.49 clean), which is why the cap experiment is sound and
/// its timing column is not.
///
/// Turn on to re-measure batch shape; leave off for anything timed. A timing arm should also
/// silence the pre-existing `local delivery ok` line, which is the heavier of the two.
/// Whether the per-injection delivery milestones are printed.
///
/// **Counters stay live; only the printing is gated.** This one line is 7,631 of a boot's 15,336
/// console lines -- over half of everything the guest prints -- and a console write is a syscall
/// on the delivery path. It is diagnostic scaffolding from the lost-FIN investigation, not
/// product behaviour, and it has to be off for any arm whose timing is going to be read.
///
/// The correctness alarms are deliberately NOT gated: `FRAMING BROKEN` and the local-drop and
/// TXFIN reports are silent when nothing is wrong (0, 0 and 49 lines respectively in a clean
/// round), so they cost nothing and must keep firing.
const REPORT_DELIVERY_MILESTONES: bool = false;

const REPORT_BATCH_SHAPE: bool = false;

/// Frames by class, and the same restricted to frames that rode a **one-frame** message.
///
/// The second table is the one that matters: if the singletons are overwhelmingly ACKs, the
/// residual ratio is not headroom and the search stops here rather than continuing to chase it.
static CLS_TOTAL: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static CLS_ALONE: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Delivered batch lengths. Fixed at 17 entries rather than `MAX_PACKETS_SET + 1` so the report
/// line has the same shape in both arms of the cap experiment and the two can be diffed directly.
static INJECT_HIST: [AtomicU64; 17] = [const { AtomicU64::new(0) }; 17];

/// Format the per-octet tables for a milestone snapshot.
fn fmt_octets(t: &[AtomicU64; 256]) -> String {
    let mut s = String::new();
    for (i, c) in t.iter().enumerate() {
        let v = c.load(Ordering::Relaxed);
        if v != 0 {
            s.push_str(&format!(" .{}={}", i, v));
        }
    }
    s
}

/// Account one successfully injected frame. Milestones are *appended*, never logged: see
/// `deliver_local` for why nothing on this path may write to the console.
fn note_inject_ok(frame: &[u8], batch_len: usize, notes: &mut Vec<String>) {
    if REPORT_BATCH_SHAPE {
        INJECT_HIST[batch_len.min(16)].fetch_add(1, Ordering::Relaxed);
        let cls = classify_frame(frame) as usize;
        CLS_TOTAL[cls].fetch_add(1, Ordering::Relaxed);
        if batch_len == 1 {
            CLS_ALONE[cls].fetch_add(1, Ordering::Relaxed);
        }
    }
    if let Some((src, dst)) = ipv4_src_dst_octets(frame) {
        // Per-address progress on powers of two of that address's own count -- monotonic, so
        // ".106 reached 16" is a fact about the boot regardless of where the round stopped. A
        // global snapshot cannot answer "how much arrived from .X": whether .X appears depends on
        // when the round ended, and that once made a *passing* round look like proof of absence.
        let sv = LOCAL_RX_BY_SRC[src as usize].fetch_add(1, Ordering::Relaxed) + 1;
        if sv.is_power_of_two() {
            notes.push(format!("local src .{} reached {}", src, sv));
        }
        let dv = LOCAL_RX_BY_DST[dst as usize].fetch_add(1, Ordering::Relaxed) + 1;
        if dv.is_power_of_two() {
            notes.push(format!("local dst .{} reached {}", dst, dv));
        }
    }
    let n = LOCAL_RX_OK.fetch_add(1, Ordering::Relaxed) + 1;
    // Every 16, not only powers of two: a boot injects 250-500 frames, so a power-of-two stride
    // lands its last snapshot at 256 and truncates the tail this counter exists to measure.
    if REPORT_DELIVERY_MILESTONES && (n % 16 == 0 || n.is_power_of_two()) {
        let (ipv4, arp, bad, other, noteth) = (
            LOCAL_RX_IPV4.load(Ordering::Relaxed),
            LOCAL_RX_ARP.load(Ordering::Relaxed),
            LOCAL_RX_BAD_IPV4.load(Ordering::Relaxed),
            LOCAL_RX_OTHER_ET.load(Ordering::Relaxed),
            LOCAL_RX_NOT_ETH.load(Ordering::Relaxed),
        );
        let (bf, bm) = (
            BATCH_FRAMES.load(Ordering::Relaxed),
            BATCH_MSGS.load(Ordering::Relaxed),
        );
        notes.push(format!(
            "local delivery ok: {} injected; accounted {} (ipv4 {} arp {} badip {} otherET {} \
             noteth {} lastET {:#06x}); batch {}f/{}m mean {:.2}; src:{} dst:{}",
            n,
            ipv4 + arp + bad + other + noteth,
            ipv4,
            arp,
            bad,
            other,
            noteth,
            LOCAL_RX_LAST_ETHERTYPE.load(Ordering::Relaxed),
            bf,
            bm,
            if bm == 0 { 0.0 } else { bf as f64 / bm as f64 },
            fmt_octets(&LOCAL_RX_BY_SRC),
            fmt_octets(&LOCAL_RX_BY_DST)
        ));
        if !REPORT_BATCH_SHAPE {
            return;
        }
        let mut hist = String::new();
        for (k, c) in INJECT_HIST.iter().enumerate() {
            let v = c.load(Ordering::Relaxed);
            if v != 0 {
                hist.push_str(&format!(" {}:{}", k, v));
            }
        }
        let ld = |t: &[AtomicU64; 4]| {
            (
                t[0].load(Ordering::Relaxed),
                t[1].load(Ordering::Relaxed),
                t[2].load(Ordering::Relaxed),
                t[3].load(Ordering::Relaxed),
            )
        };
        let (td, ta, tr, to) = ld(&CLS_TOTAL);
        let (sd, sa, sr, so) = ld(&CLS_ALONE);
        notes.push(format!(
            "batchshape cap={} hist{}; class data={} ack={} arp={} other={}; \
             alone data={} ack={} arp={} other={}",
            BATCH_MAX, hist, td, ta, tr, to, sd, sa, sr, so
        ));
    }
}

/// Account one frame dropped because the target's rx pool was empty.
///
/// Dropping is the right backpressure response and a real NIC does the same. What is *not* true is
/// the rationale this path used to carry ("the sender will retransmit"): that holds for TCP and
/// fails for UDP, and this path carries both. A datagram dropped here is gone and the sender is
/// never told, so it is counted rather than lost silently. Retrying is not an option: the target
/// drains on its own thread, which needs the handles lock the caller is holding.
fn note_inject_drop(frame: &[u8], target: EthernetAddress, notes: &mut Vec<String>) {
    if let Some((src, dst)) = ipv4_src_dst_octets(frame) {
        let dv = LOCAL_RX_DROPS_BY_DST[dst as usize].fetch_add(1, Ordering::Relaxed) + 1;
        if dv.is_power_of_two() {
            notes.push(format!("local DROP dst .{} reached {} (from .{})", dst, dv, src));
        }
    }
    let n = LOCAL_RX_DROPS.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_power_of_two() {
        notes.push(format!(
            "dropped local frame for {} (rx pool exhausted); {} dropped so far",
            target, n
        ));
    }
}

/// Deliver this poll's whole egress batch to every local target, in one queue message per
/// `MAX_PACKETS_SET` frames per target.
///
/// Replaces the old per-frame `inject_local`. The batch is already in hand -- `client_thread`
/// drains the client's tx queue into `pending` and drops its own `ep` before calling here -- so
/// the count is known at the call and nothing is deferred, timed, or flushed. A target with one
/// frame gets a one-frame message; the "move one if that is all there is" case is the same code
/// path with a shorter slice.
///
/// Takes the handles lock and then each target's `ep`, the order `device_thread` also uses. The
/// caller must not hold its own `ep`: two client threads cross-injecting while each held its own
/// would deadlock.
///
/// **Nothing here writes to the console.** Milestones accumulate in `notes` and are emitted after
/// both locks drop. A console write is a syscall, and doing one under these locks stalls every
/// other client thread behind it -- the probe that logged on this path once took the suite from
/// 13/50 failures to 50/50. Batching sharpens that: one `ep` acquisition now covers up to
/// `MAX_PACKETS_SET` frames, so a burst that used to emit one line per hold could emit eight.
fn deliver_local(pending: &[(Vec<u8>, Dest)], sender: EthernetAddress) {
    if pending.is_empty() {
        return;
    }
    let mut notes: Vec<String> = Vec::new();
    // Whether any live client matched this frame's destination -- distinct from whether it was
    // delivered. A frame that matched but hit an empty pool is a drop, already counted as one, and
    // must not also count as "matched nobody".
    let mut matched = vec![false; pending.len()];

    let handles = NETINFO.get().unwrap().handles.lock().unwrap();
    for (_, _, client) in handles.handles() {
        let hw = client.addr.hwaddr();
        if hw == sender {
            continue;
        }
        // This target's frames **in the order the sender emitted them**. Order is load-bearing:
        // delivering all unicast and then all floods would reorder a stream TCP expects in
        // sequence, and a single-client test cannot see it because floods match nobody.
        let idx: Vec<usize> = pending
            .iter()
            .enumerate()
            .filter(|(_, (_, d))| match d {
                Dest::Local(dst) => *dst == hw,
                Dest::Flood => true,
                Dest::Device => false,
            })
            .map(|(i, _)| i)
            .collect();
        if idx.is_empty() {
            continue;
        }
        for i in &idx {
            matched[*i] = true;
        }

        let mut ep = client.ep.lock().unwrap();
        for chunk in idx.chunks(BATCH_MAX) {
            let frames: Vec<&[u8]> = chunk.iter().map(|i| pending[*i].0.as_slice()).collect();
            let n = ep.inject(&frames);
            BATCH_MSGS.fetch_add(1, Ordering::Relaxed);
            BATCH_FRAMES.fetch_add(n as u64, Ordering::Relaxed);
            for (k, i) in chunk.iter().enumerate() {
                if k < n {
                    note_inject_ok(&pending[*i].0, n, &mut notes);
                } else {
                    note_inject_drop(&pending[*i].0, hw, &mut notes);
                }
            }
        }
    }
    drop(handles);

    // A `Dest::Local` frame that matched no live client is the one delivery outcome producing no
    // record at all: `classify` decided Local from a snapshot taken before this client's `ep` was
    // held, and a sibling that went away since leaves the frame with nowhere to go -- it is not
    // injected, and being Local it never reaches the NIC either.
    for (i, (frame, dest)) in pending.iter().enumerate() {
        if matches!(dest, Dest::Local(_)) && !matched[i] {
            LOCAL_NOMATCH.fetch_add(1, Ordering::Relaxed);
            if tcp_fin_src_octet(frame).is_some() {
                LOCAL_NOMATCH_FIN.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    for n in notes {
        tracing::warn!("{}", n);
    }
}

fn client_thread(client: Arc<Client>) {
    let device = NETINFO.get().unwrap().device.clone();
    let tx_po = client.ep.lock().unwrap().client_tx_packet_object().clone();
    let sender = client.addr.hwaddr();
    // Frames destined for a sibling, copied out of the packet object so this client's `ep` lock
    // can be dropped before any target's is taken. Note the copy is a whole packet slot, not a
    // frame: this protocol carries no length, so both directions already hand smoltcp the full
    // slot and let the IP/ARP header lengths govern.
    let mut pending: Vec<(Vec<u8>, Dest)> = Vec::new();
    let mut local_macs: Vec<EthernetAddress> = Vec::new();
    while client.active() {
        // Snapshot sibling MACs *before* taking our own `ep`. Reading them inside the frame loop
        // would mean holding `ep` while taking the handles lock, inverting device_thread's
        // handles-then-ep order. A client that opens after this snapshot is simply not local yet,
        // so a frame for it goes out the NIC and is dropped; ARP retries.
        local_macs.clear();
        local_macs.extend(
            NETINFO
                .get()
                .unwrap()
                .handles
                .lock()
                .unwrap()
                .handles()
                .map(|(_, _, c)| c.addr.hwaddr())
                .filter(|a| *a != sender),
        );

        let mut ep = client.ep.lock().unwrap();
        while let Some((rx, _tx)) = ep.receive(Instant::now()) {
            let packet = rx.packet;
            rx.consume(|buf| {
                if false {
                    let f = EthernetFrame::new_unchecked(&*buf);
                    let pp = PrettyPrinter::<EthernetFrame<&[u8]>>::print(&f);
                    eprintln!("client thread got {}", pp);
                }
                let dest = classify(buf, &local_macs);
                if let Some((src, dst)) = tcp_fin_src_octet(buf) {
                    TX_FIN_BY_SRC[src as usize].fetch_add(1, Ordering::Relaxed);
                    TX_FIN_BY_DST[dst as usize].fetch_add(1, Ordering::Relaxed);
                    match dest {
                        Dest::Local(_) => &TX_FIN_LOCAL,
                        Dest::Device => &TX_FIN_DEVICE,
                        Dest::Flood => &TX_FIN_FLOOD,
                    }
                    .fetch_add(1, Ordering::Relaxed);
                    // Counters only here -- NO logging. This closure runs inside `rx.consume`
                    // while `client.ep` is locked, and every pre-existing log call in this file
                    // fires only after `drop(ep)`. A console write is a syscall; doing one under
                    // that lock stalls every other client thread behind it. The first version of
                    // this probe logged here and took the test suite from 13/50 failures to
                    // **50/50** -- an instrument that destroyed the behaviour it was measuring.
                    // The report is emitted below, outside the lock.
                }
                // The NIC path keeps the zero-copy handoff of the client's own tx packet; only
                // frames that stay on-box are copied.
                if !matches!(dest, Dest::Local(_)) {
                    let real = frame_len(buf);
                    let sent = if TRIM_TX_TO_FRAME { real } else { buf.len() };
                    note_dev_tx(sent, real);
                    let tx = TxBuffer::from_packet(tx_po.clone(), sent, packet, false);
                    device.transmit(tx);
                }
                if !matches!(dest, Dest::Device) {
                    let n = if TRIM_TX_TO_FRAME {
                        frame_len(buf)
                    } else {
                        buf.len()
                    };
                    pending.push((buf[..n].to_vec(), dest));
                }
            })
        }

        let rx_waiter = ep.rx_waiter();
        let comp_space_waiter = ep.completion_space_waiter();
        let has_pending_msg = ep.has_pending_msg_from_client();
        drop(ep);

        // Unconditional totals on a fixed stride, not milestones: the question is whether a
        // handoff to the client's rx ring ever failed, and a report that only appears when the
        // count is nonzero cannot say "zero" -- which is the state `POLLQ ... reached N` has been
        // in for ~65,000 sweep logs.
        {
            static PQ_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            if PQ_TICK.fetch_add(1, Ordering::Relaxed) % 64 == 0 {
                twizzler_net::report_pollq();
            }
        }

        // Outside the ep lock, alongside the other counter reports. Power-of-two milestones on
        // the total, so a busy run cannot flood the console either.
        {
            let fins = TX_FIN_LOCAL.load(Ordering::Relaxed)
                + TX_FIN_DEVICE.load(Ordering::Relaxed)
                + TX_FIN_FLOOD.load(Ordering::Relaxed);
            if fins > 0 && TX_FIN_REPORTED.swap(fins, Ordering::Relaxed) != fins {
                tracing::warn!(
                    "TXFIN total={} local={} device={} flood={} by_src .100={} .105={} \
                     nomatch={} nomatch_fin={} by_dst .105={} .100={}",
                    fins,
                    TX_FIN_LOCAL.load(Ordering::Relaxed),
                    TX_FIN_DEVICE.load(Ordering::Relaxed),
                    TX_FIN_FLOOD.load(Ordering::Relaxed),
                    TX_FIN_BY_SRC[100].load(Ordering::Relaxed),
                    TX_FIN_BY_SRC[105].load(Ordering::Relaxed),
                    LOCAL_NOMATCH.load(Ordering::Relaxed),
                    LOCAL_NOMATCH_FIN.load(Ordering::Relaxed),
                    TX_FIN_BY_DST[105].load(Ordering::Relaxed),
                    TX_FIN_BY_DST[100].load(Ordering::Relaxed),
                );
            }
        }

        // Permanent, not scaffolding: silent while framing is correct, loud the moment it is not.
        // The suite structurally cannot fail on oversized frames -- SLIRP parses by header and
        // ignores the tail -- so without this the defect could return and every test would stay
        // green. Emitted here rather than at the tx site because that runs under `client.ep`, and
        // a console write under that lock once took this suite from 13/50 failures to 50/50.
        {
            let bad = DEV_TX_OVERSIZED.load(Ordering::Relaxed);
            if bad > 0 && DEV_TX_REPORTED.swap(bad, Ordering::Relaxed) != bad {
                tracing::warn!(
                    "FRAMING BROKEN: handed the NIC {} frame(s) above the {}-byte MTU (max {}); \
                     {} of {} bytes sent were slot padding",
                    bad,
                    MTU,
                    DEV_TX_MAXLEN.load(Ordering::Relaxed),
                    DEV_TX_SENTBYTES
                        .load(Ordering::Relaxed)
                        .saturating_sub(DEV_TX_FRAMEBYTES.load(Ordering::Relaxed)),
                    DEV_TX_SENTBYTES.load(Ordering::Relaxed),
                );
            }
        }

        // One pass over the whole batch, grouped per target, rather than one pass per frame.
        // A flood matching nobody is the ordinary one-client case, not a loss.
        deliver_local(&pending, sender);
        pending.clear();

        if has_pending_msg {
            continue;
        }

        // Every word this thread can be woken by, and no others. It reads client submissions
        // (rx_waiter) and writes completions (comp_space_waiter, only while one is owed). It also
        // reads client_rx completions in `inject`, but never retries on them, so waking for a
        // packet reclaim would be churn with nothing to do -- `inject` drains them itself.
        let mut sleeps = vec![ThreadSync::new_sleep(rx_waiter)];
        if let Some(w) = comp_space_waiter {
            sleeps.push(ThreadSync::new_sleep(w));
        }
        let _ = sys_thread_sync(&mut sleeps, None);
    }
}
