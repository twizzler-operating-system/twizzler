use std::sync::Arc;

use secgate::TwzError;
use smoltcp::{
    phy::{Device, TxToken},
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

impl NetDriver for DeviceWrapper<TwizzlerTransport> {
    fn init(device: Device) -> Result<Self, TwzError> {
        todo!()
    }

    fn device(&self) -> &Device {
        todo!()
    }

    fn device_mut(&mut self) -> &mut Device {
        todo!()
    }

    fn setup_rx_queue(&mut self, len: usize) -> Result<QueueHandle, TwzError> {
        todo!()
    }

    fn destroy_rx_queue(&mut self, queue: QueueHandle) -> Result<(), TwzError> {
        todo!()
    }

    fn setup_tx_queue(&mut self, len: usize) -> Result<QueueHandle, TwzError> {
        todo!()
    }

    fn destroy_tx_queue(&mut self, queue: QueueHandle) -> Result<(), TwzError> {
        todo!()
    }

    fn tx_queues(&self) -> Vec<QueueHandle> {
        todo!()
    }

    fn rx_queues(&self) -> Vec<QueueHandle> {
        todo!()
    }

    fn mac_address(&self, queue: QueueHandle) -> Result<[u8; 6], TwzError> {
        todo!()
    }

    fn recv_packets(
        &mut self,
        queue: QueueHandle,
        packets: &mut [Packet],
    ) -> Result<usize, TwzError> {
        todo!()
    }

    fn send_packets(
        &mut self,
        queue: QueueHandle,
        packets: &mut [Packet],
    ) -> Result<usize, TwzError> {
        todo!()
    }

    fn has_work(&self, queue: QueueHandle) -> WorkItems {
        todo!()
    }

    fn waitpoint(&self, queue: QueueHandle) -> twizzler_abi::syscall::ThreadSync {
        todo!()
    }
}

fn handle_work(
    device: &mut Box<dyn NetDriver>,
    queue: QueueHandle,
    work: WorkItems,
    inject: impl FnMut(&[Packets]) -> Result<usize, TwzError>,
) {
    if work.contains(WorkItems::RX_READY) {
        let mut packets = [Packet::default(); 32];
        if let Ok(count) = device.recv_packets(queue, &mut packets) {
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
    inject: impl FnMut(&[Packets]) -> Result<usize, TwzError>,
) {
    let rx_queues = device.rx_queues();
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
                handle_work(&mut device, *q, work, inject);
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
                    handle_work(&mut device, *q, work, inject);
                }
                waitpoints[i] = wp;
            }
            if !any_ready {
                let _ = sys_thread_sync(waitpoints.as_mut_slice(), None);
            }
        }
    }
}
