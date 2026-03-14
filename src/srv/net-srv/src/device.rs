use std::sync::Arc;

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

pub fn device_thread(device: DeviceWrapper<TwizzlerTransport>) {
    loop {
        while let Some(mut rx) = device.get_rx() {
            let buf = rx.packet_mut();
            if false {
                let f = EthernetFrame::new_unchecked(&mut *buf);
                let pp = PrettyPrinter::<EthernetFrame<&mut [u8]>>::print(&f);
                eprintln!("device thread got {}", pp);
            }
            let handles = NETINFO.get().unwrap().handles.lock().unwrap();
            for (_, _, client) in handles.handles() {
                let mut ep = client.ep.lock().unwrap();
                let ctx = ep.transmit(Instant::now()).unwrap();
                ctx.consume(buf.len(), |cbuf| cbuf.copy_from_slice(buf));
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
