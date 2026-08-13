use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
};

use smoltcp::{
    phy::{Device, RxToken, TxToken},
    time::Instant,
    wire::{EthernetAddress, EthernetFrame, PrettyPrinter},
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
        Dest::Device
    }
}

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
        // No free rx packet means that client is backed up. Dropping is what a real NIC does under
        // the same pressure, and the sender will retransmit.
        if let Some(tx) = ep.transmit(Instant::now()) {
            tx.consume(frame.len(), |b: &mut [u8]| b.copy_from_slice(frame));
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
