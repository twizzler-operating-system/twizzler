use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    thread::JoinHandle,
};

use smoltcp::{
    phy::{Device, RxToken},
    time::Instant,
    wire::{ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol, Ipv4Packet, Ipv6Packet},
};
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync, ThreadSyncWake};
use twizzler_net::{NetServer, MAX_PACKETS_SET};
use virtio_net::TxBuffer;

use crate::{addr::ClientAddr, ADDRS, NETINFO, PORTS};

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
        let weak = Arc::downgrade(&client);
        let jh = std::thread::Builder::new()
            .name("net-client".into())
            .spawn(move || client_thread(weak))
            .unwrap();
        client.jh.set(jh).unwrap();
        client
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Retire the client: hand its ports and address back and get its thread out of its rx
    /// sleep, which a dead compartment would otherwise never end. Idempotent, so the explicit
    /// drop gate, the dead-compartment sweep, and `Drop` can all call it.
    pub fn teardown(&self) {
        if self.active.swap(false, Ordering::SeqCst) {
            for (port, _) in self.ports.lock().unwrap().drain() {
                PORTS.get().unwrap().return_port(port);
            }
            ADDRS.get().unwrap().release(self.addr);
        }
        let waiter = self.ep.lock().unwrap().rx_waiter();
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                waiter.reference,
                usize::MAX,
            ))],
            None,
        );
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.teardown();
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

/// Frames handed to the NIC above the MTU we advertise, and the largest seen.
///
/// The test suite cannot go red on this defect: QEMU's SLIRP backend parses by header and ignores
/// the tail, so an oversized frame is still delivered and every test passes. This is the only
/// thing that notices the framing regressing.
static DEV_TX_OVERSIZED: AtomicU64 = AtomicU64::new(0);
static DEV_TX_MAXLEN: AtomicU64 = AtomicU64::new(0);
static DEV_TX_REPORTED: AtomicU64 = AtomicU64::new(0);

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
        Dest::Device
    }
}

/// Frames dropped by `deliver_local` because the target's rx pool had no free packet.
static LOCAL_RX_DROPS: AtomicU64 = AtomicU64::new(0);

/// Deliver this poll's whole egress batch to every local target, in one queue message per
/// `MAX_PACKETS_SET` frames per target.
///
/// The batch is already in hand -- `client_thread` drains the client's tx queue into `pending` and
/// drops its own `ep` before calling here -- so the count is known at the call and nothing is
/// deferred, timed, or flushed.
///
/// Takes the handles lock and then each target's `ep`, the order `device_thread` also uses. The
/// caller must not hold its own `ep`: two client threads cross-injecting while each held its own
/// would deadlock.
///
/// Nothing here writes to the console under the locks: a console write is a syscall, and doing
/// one under these locks stalls every other client thread behind it.
fn deliver_local(pending: &[(Vec<u8>, Dest)], sender: EthernetAddress) {
    if pending.is_empty() {
        return;
    }
    let mut dropped = 0u64;

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

        let mut ep = client.ep.lock().unwrap();
        for chunk in idx.chunks(MAX_PACKETS_SET) {
            let frames: Vec<&[u8]> = chunk.iter().map(|i| pending[*i].0.as_slice()).collect();
            // A short return is a backed-up client; drop rather than block the switch. Retrying
            // would deadlock: the target drains on its own thread, which needs the handles lock
            // held here.
            let n = ep.inject(&frames);
            dropped += (chunk.len() - n) as u64;
        }
    }
    drop(handles);

    if dropped > 0 {
        let n = LOCAL_RX_DROPS.fetch_add(dropped, Ordering::Relaxed) + dropped;
        if n.is_power_of_two() {
            tracing::warn!(
                "dropped local frames (rx pool exhausted); {} dropped so far",
                n
            );
        }
    }
}

/// Holds the client only while working. Sleeping on a `Weak` lets the handle table's last strong
/// reference drop the client, whose `Drop` wakes this thread to find the upgrade failing.
fn client_thread(weak: Weak<Client>) {
    let Some(client) = weak.upgrade() else {
        return;
    };
    let device = NETINFO.get().unwrap().device.clone();
    let tx_po = client.ep.lock().unwrap().client_tx_packet_object().clone();
    let sender = client.addr.hwaddr();
    drop(client);
    // Frames destined for a sibling, copied out of the packet object so this client's `ep` lock
    // can be dropped before any target's is taken.
    let mut pending: Vec<(Vec<u8>, Dest)> = Vec::new();
    // Recycled bodies for `pending`: the copy out of the client's tx packet has to outlive the
    // `ep` lock, but the heap alloc/free per frame does not.
    let mut spare: Vec<Vec<u8>> = Vec::new();
    let mut local_macs: Vec<EthernetAddress> = Vec::new();
    loop {
        let Some(client) = weak.upgrade() else {
            break;
        };
        if !client.active() {
            break;
        }
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
                let dest = classify(buf, &local_macs);
                let len = frame_len(buf);
                // The NIC path keeps the zero-copy handoff of the client's own tx packet; only
                // frames that stay on-box are copied.
                if !matches!(dest, Dest::Local(_)) {
                    if len > MTU {
                        DEV_TX_OVERSIZED.fetch_add(1, Ordering::Relaxed);
                        DEV_TX_MAXLEN.fetch_max(len as u64, Ordering::Relaxed);
                    }
                    let tx = TxBuffer::from_packet(tx_po.clone(), len, packet, false);
                    device.transmit(tx);
                }
                if !matches!(dest, Dest::Device) {
                    let mut body = spare.pop().unwrap_or_default();
                    body.clear();
                    body.extend_from_slice(&buf[..len]);
                    pending.push((body, dest));
                }
            })
        }

        let rx_waiter = ep.rx_waiter();
        let comp_space_waiter = ep.completion_space_waiter();
        let has_pending_msg = ep.has_pending_msg_from_client();
        drop(ep);

        // Silent while framing is correct, loud the moment it is not. Emitted here rather than at
        // the tx site because that runs under `client.ep`.
        let bad = DEV_TX_OVERSIZED.load(Ordering::Relaxed);
        if bad > 0 && DEV_TX_REPORTED.swap(bad, Ordering::Relaxed) != bad {
            tracing::warn!(
                "FRAMING BROKEN: handed the NIC {} frame(s) above the {}-byte MTU (max {})",
                bad,
                MTU,
                DEV_TX_MAXLEN.load(Ordering::Relaxed),
            );
        }

        deliver_local(&pending, sender);
        for (body, _) in pending.drain(..) {
            if spare.len() < 64 {
                spare.push(body);
            }
        }

        if has_pending_msg {
            continue;
        }

        // Every word this thread can be woken by, and no others. It reads client submissions
        // (rx_waiter) and writes completions (comp_space_waiter, only while one is owed). It also
        // reads client_rx completions in `inject`, but never retries on them, so waking for a
        // packet reclaim would be churn with nothing to do -- `inject` drains them itself.
        let mut sleeps = [
            ThreadSync::new_sleep(rx_waiter),
            ThreadSync::new_sleep(rx_waiter),
        ];
        let n = if let Some(w) = comp_space_waiter {
            sleeps[1] = ThreadSync::new_sleep(w);
            2
        } else {
            1
        };
        drop(client);
        let _ = sys_thread_sync(&mut sleeps[..n], None);
    }
}
