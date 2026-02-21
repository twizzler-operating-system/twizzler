use smoltcp::{
    phy::{Device, TxToken},
    time::Instant,
    wire::{EthernetFrame, PrettyPrinter},
};
use twizzler_abi::syscall::sys_thread_sync;
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
