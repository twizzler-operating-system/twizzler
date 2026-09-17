use secgate::TwzError;
use smoltcp::wire::EthernetFrame;
use twizzler_abi::syscall::sys_thread_sync;
use twizzler_net::drivers::{NetDriver, Packet, QueueHandle, WorkItems};
use virtio_net::{DeviceWrapper, TwizzlerTransport};

use crate::NETINFO;

pub fn device_thread(device: DeviceWrapper<TwizzlerTransport>) {
    loop {
        while let Some(mut rx) = device.get_rx() {
            let buf = rx.packet_mut();
            // A switch forwards a unicast frame to the port owning the address; flooding it to
            // every client costs one copy per client to deliver one. Broadcast and multicast still
            // go everywhere, and so does a unicast address no client claims -- that fallback keeps
            // delivery for anything we cannot attribute, so the filter can only ever narrow
            // delivery when it has positively identified the owner.
            let target = EthernetFrame::new_checked(&*buf)
                .ok()
                .map(|f| f.dst_addr())
                .filter(|d| !d.is_broadcast() && !d.is_multicast());
            let handles = NETINFO.get().unwrap().handles.lock().unwrap();
            let owner = target.filter(|t| handles.handles().any(|(_, _, c)| c.addr.hwaddr() == *t));
            for (_, _, client) in handles.handles() {
                if owner.is_some_and(|t| client.addr.hwaddr() != t) {
                    continue;
                }
                let mut ep = client.ep.lock().unwrap();
                // A client with no free rx packet is backed up; drop its copy rather than taking
                // the whole network service down with it. Unwrapping here made one wedged client
                // fatal for every other one.
                ep.inject(&[&buf[..]]);
            }
            drop(handles);
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
