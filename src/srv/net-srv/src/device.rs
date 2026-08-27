use std::sync::atomic::{AtomicU64, Ordering};

use secgate::TwzError;
use smoltcp::{
    phy::{Device as _, TxToken},
    time::Instant,
    wire::{EthernetFrame, PrettyPrinter},
};
use twizzler_abi::syscall::sys_thread_sync;
use twizzler_net::drivers::{NetDriver, Packet, QueueHandle, WorkItems};
use virtio_net::{DeviceWrapper, TwizzlerTransport};

use crate::NETINFO;

/// Whether a unicast frame goes only to the client that owns the address.
///
/// `false` reproduces the pre-fix behaviour (copy every inbound frame to every client). Named so
/// the arm a build was is one grep on the source rather than an inference from a file mtime.
const FILTER_UNICAST: bool = true;

/// Inbound frames, and copies made to deliver them.
///
/// `copies / frames` is the fanout. It is extensive in work -- it counts the same on a fast box, a
/// slow box or a contended one -- so unlike a timing it survives every measurement hazard on this
/// machine. It also carries its own control: broadcast still fans out to every client, so a run
/// that shows unicast at ~1 and flood at ~N proves the filter discriminates rather than simply
/// dropping traffic. There is deliberately no performance claim attached -- no local workload
/// reaches this path in volume, and `stdnet_test` times a GET against an internet host.
static DEV_RX_FRAMES: AtomicU64 = AtomicU64::new(0);
static DEV_RX_COPIES: AtomicU64 = AtomicU64::new(0);
static DEV_RX_TARGETED: AtomicU64 = AtomicU64::new(0);
static DEV_RX_FLOODED: AtomicU64 = AtomicU64::new(0);
static DEV_RX_REPORTED: AtomicU64 = AtomicU64::new(0);

pub fn device_thread(device: DeviceWrapper<TwizzlerTransport>) {
    loop {
        while let Some(mut rx) = device.get_rx() {
            let buf = rx.packet_mut();
            if false {
                let f = EthernetFrame::new_unchecked(&mut *buf);
                let pp = PrettyPrinter::<EthernetFrame<&mut [u8]>>::print(&f);
                eprintln!("device thread got {}", pp);
            }
            // A switch forwards a unicast frame to the port owning the address; flooding it to
            // every client costs one copy per client to deliver one. Broadcast and multicast still
            // go everywhere, and so does a unicast address no client claims -- that fallback keeps
            // today's behaviour for anything we cannot attribute, so the filter can only ever
            // narrow delivery when it has positively identified the owner.
            let target = EthernetFrame::new_checked(&*buf)
                .ok()
                .map(|f| f.dst_addr())
                .filter(|d| !d.is_broadcast() && !d.is_multicast());
            let handles = NETINFO.get().unwrap().handles.lock().unwrap();
            let owner = target.filter(|t| {
                FILTER_UNICAST && handles.handles().any(|(_, _, c)| c.addr.hwaddr() == *t)
            });
            DEV_RX_FRAMES.fetch_add(1, Ordering::Relaxed);
            if owner.is_some() {
                &DEV_RX_TARGETED
            } else {
                &DEV_RX_FLOODED
            }
            .fetch_add(1, Ordering::Relaxed);
            for (_, _, client) in handles.handles() {
                if owner.is_some_and(|t| client.addr.hwaddr() != t) {
                    continue;
                }
                let mut ep = client.ep.lock().unwrap();
                // A client with no free rx packet is backed up; drop its copy rather than taking
                // the whole network service down with it. Unwrapping here made one wedged client
                // fatal for every other one.
                if let Some(ctx) = ep.transmit(Instant::now()) {
                    DEV_RX_COPIES.fetch_add(1, Ordering::Relaxed);
                    ctx.consume(buf.len(), |cbuf| cbuf.copy_from_slice(buf));
                };
            }
            drop(handles);
            {
                let n = DEV_RX_FRAMES.load(Ordering::Relaxed);
                if n % 32 == 0 && DEV_RX_REPORTED.swap(n, Ordering::Relaxed) != n {
                    tracing::warn!(
                        "DEVRX frames={} copies={} targeted={} flooded={}",
                        n,
                        DEV_RX_COPIES.load(Ordering::Relaxed),
                        DEV_RX_TARGETED.load(Ordering::Relaxed),
                        DEV_RX_FLOODED.load(Ordering::Relaxed),
                    );
                }
            }
            device.recycle(rx);
        }

        if !device.has_work() {
            let sleep = device.get_sleep();
            if !device.has_work() {
                let _ = sys_thread_sync(&mut [sleep], None);
            }
        }
    }
}

fn handle_work(
    device: &mut Box<dyn NetDriver>,
    queue: QueueHandle,
    work: WorkItems,
    inject: &mut impl FnMut(&[Packet]) -> Result<usize, TwzError>,
    packets: &mut [Packet],
) {
    if work.contains(WorkItems::RX_READY) {
        if let Ok(count) = device.recv_packets(queue, packets) {
            let mut injected = 0;
            while injected < count {
                if let Ok(injected_count) = inject(&packets[injected..count]) {
                    injected += injected_count;
                } else {
                    break;
                }
            }
        }
    }
    if work.contains(WorkItems::STATUS_CHANGE) {
        tracing::info!("link status change");
    }
    if work.contains(WorkItems::TX_ERROR) {
        tracing::error!("tx error");
    }
    if work.contains(WorkItems::RX_ERROR) {
        tracing::error!("rx error");
    }
}

pub fn device_thread_main(
    mut device: Box<dyn NetDriver>,
    mut inject: impl FnMut(&[Packet]) -> Result<usize, TwzError>,
) {
    let rx_queues = device.rx_queues();
    let mut packets = vec![Packet::default(); 32];
    let mut waitpoints = rx_queues
        .iter()
        .map(|q| device.waitpoint(*q))
        .collect::<Vec<_>>();
    let mut counter = 0;
    loop {
        for q in &rx_queues {
            let work = device.has_work(*q);
            if !work.is_empty() {
                counter = 100;
                handle_work(&mut device, *q, work, &mut inject, packets.as_mut_slice());
            }
        }
        if counter > 0 {
            counter -= 1;
        } else {
            let mut any_ready = false;
            for (i, q) in rx_queues.iter().enumerate() {
                let wp = device.waitpoint(*q);
                let work = device.has_work(*q);
                if !work.is_empty() {
                    any_ready = true;
                    handle_work(&mut device, *q, work, &mut inject, packets.as_mut_slice());
                }
                waitpoints[i] = wp;
            }
            if !any_ready {
                let _ = sys_thread_sync(waitpoints.as_mut_slice(), None);
            }
        }
    }
}
