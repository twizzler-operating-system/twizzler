use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
};

use smoltcp::{
    phy::{Device, RxToken, TxToken},
    time::Instant,
    wire::{EthernetAddress, EthernetFrame, EthernetProtocol, Ipv4Packet, PrettyPrinter},
};
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync};
use twizzler_net::NetServer;
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
#[derive(Clone, Copy)]
enum Dest {
    /// Off-box, out the NIC -- the ordinary case.
    Device,
    /// Another client on this host, addressed by its MAC.
    Local(EthernetAddress),
    /// Broadcast/multicast: every other client *and* the NIC.
    Flood,
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
fn inject_local(frame: &[u8], sender: EthernetAddress, f: impl Fn(&Client) -> bool) {
    let handles = NETINFO.get().unwrap().handles.lock().unwrap();
    for (_, _, client) in handles.handles() {
        if client.addr.hwaddr() == sender || !f(client) {
            continue;
        }
        let mut ep = client.ep.lock().unwrap();
        match ep.transmit(Instant::now()) {
            Some(tx) => {
                tx.consume(frame.len(), |b: &mut [u8]| b.copy_from_slice(frame));
                if let Some((src, dst)) = ipv4_src_dst_octets(frame) {
                    // Per-address progress, reported on powers of two of that address's own
                    // count. This is deliberately NOT tied to the global snapshot below, because
                    // the global one cannot answer "how much arrived from .X": a boot injects ~350
                    // frames and a single test contributes its handful in the last thirty, so
                    // whether .X appears depends on where the round happened to stop. Measured,
                    // not feared -- source .106 first appeared at injection 320-336 in passing
                    // rounds and was missing from the final snapshot of two *passing* rounds, so a
                    // failing round stopping at 304 would have looked like proof of absence.
                    //
                    // Keyed on the address's own count, the report is immune to that: the counters
                    // are monotonic, so ".106 reached 16" is a fact about the boot regardless of
                    // when it ended. Powers of two also separate the cases that matter here --
                    // "twenty arrived" from "one ARP-driven frame arrived" -- which bare presence
                    // cannot. Cost is a compare on the value `fetch_add` already returns.
                    let sv = LOCAL_RX_BY_SRC[src as usize].fetch_add(1, Ordering::Relaxed) + 1;
                    if sv.is_power_of_two() {
                        tracing::warn!("local src .{} reached {}", src, sv);
                    }
                    let dv = LOCAL_RX_BY_DST[dst as usize].fetch_add(1, Ordering::Relaxed) + 1;
                    if dv.is_power_of_two() {
                        tracing::warn!("local dst .{} reached {}", dst, dv);
                    }
                }
                let n = LOCAL_RX_OK.fetch_add(1, Ordering::Relaxed) + 1;
                // Every 16, not on powers of two. Powers of two truncate the tail: a boot injects
                // 250-500 frames, so the last snapshot lands at 256 and everything after it is
                // never reported. That silently hid the traffic this counter exists to measure --
                // source .106 was absent from the final snapshot of a *passing* round, which would
                // have read as "absent in both arms" and settled nothing.
                //
                // A fixed stride still conditions each snapshot on the same total injection count,
                // so failing and passing rounds compare at equal denominators rather than equal
                // wall-clock. And because these counters are monotonic, "did .106 ever receive
                // anything" is answered by *any* snapshot after its traffic, not only the last.
                if n % 16 == 0 || n.is_power_of_two() {
                    let fmt = |t: &[AtomicU64; 256]| {
                        let mut s = String::new();
                        for (i, c) in t.iter().enumerate() {
                            let v = c.load(Ordering::Relaxed);
                            if v != 0 {
                                s.push_str(&format!(" .{}={}", i, v));
                            }
                        }
                        s
                    };
                    let (ipv4, arp, bad, other, noteth) = (
                        LOCAL_RX_IPV4.load(Ordering::Relaxed),
                        LOCAL_RX_ARP.load(Ordering::Relaxed),
                        LOCAL_RX_BAD_IPV4.load(Ordering::Relaxed),
                        LOCAL_RX_OTHER_ET.load(Ordering::Relaxed),
                        LOCAL_RX_NOT_ETH.load(Ordering::Relaxed),
                    );
                    let accounted = ipv4 + arp + bad + other + noteth;
                    tracing::warn!(
                        "local delivery ok: {} injected; accounted {} (ipv4 {} arp {} badip {} \
                         otherET {} noteth {} lastET {:#06x}); src:{} dst:{}",
                        n,
                        accounted,
                        ipv4,
                        arp,
                        bad,
                        other,
                        noteth,
                        LOCAL_RX_LAST_ETHERTYPE.load(Ordering::Relaxed),
                        fmt(&LOCAL_RX_BY_SRC),
                        fmt(&LOCAL_RX_BY_DST)
                    );
                }
            }
            // No free rx packet means that client is backed up, and dropping is the right
            // backpressure response -- a real NIC does the same. What is *not* true is the
            // rationale this arm used to carry ("the sender will retransmit"): that holds for TCP
            // and fails for UDP, and this path carries both. A datagram dropped here is gone, and
            // the sender is never told. So it is counted and reported rather than lost silently.
            //
            // Reported on powers of two because the alternative shapes the thing it measures: this
            // runs under the handles lock on the local delivery path, and a line per drop would
            // turn a burst into a stall. Retrying here is not an option for the same reason -- the
            // target drains on its own thread, and `client_thread` needs `handles` to make
            // progress, so waiting for it while holding that lock is a deadlock, not a fix.
            None => {
                let n = LOCAL_RX_DROPS.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    tracing::warn!(
                        "dropped local frame for {} (rx pool exhausted); {} dropped so far",
                        client.addr.hwaddr(),
                        n
                    );
                }
            }
        };
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
                // The NIC path keeps the zero-copy handoff of the client's own tx packet; only
                // frames that stay on-box are copied.
                if !matches!(dest, Dest::Local(_)) {
                    let tx = TxBuffer::from_packet(tx_po.clone(), buf.len(), packet, false);
                    device.transmit(tx);
                }
                if !matches!(dest, Dest::Device) {
                    pending.push((buf.to_vec(), dest));
                }
            })
        }

        let rx_waiter = ep.rx_waiter();
        let has_pending_msg = ep.has_pending_msg_from_client();
        drop(ep);

        for (frame, dest) in pending.drain(..) {
            match dest {
                Dest::Local(dst) => inject_local(&frame, sender, |c| c.addr.hwaddr() == dst),
                Dest::Flood => inject_local(&frame, sender, |_| true),
                Dest::Device => unreachable!("never queued"),
            }
        }

        if has_pending_msg {
            continue;
        }

        let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(rx_waiter)], None);
    }
}
