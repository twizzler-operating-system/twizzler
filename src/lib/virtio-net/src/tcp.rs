//! Simple echo server over TCP.
//!
//! Ref: <https://github.com/smoltcp-rs/smoltcp/blob/master/examples/server.rs>
use std::sync::{Arc, Mutex};

use smoltcp::wire::EthernetAddress;
use twizzler_abi::syscall::{ObjectCreate, ThreadSync};
use twizzler_net::packet::PacketObject;
use virtio_drivers::{
    device::net::{RxBuffer, TxBuffer, VirtIONet},
    transport::Transport,
};

use crate::{hal::TwzHal, transport::TwizzlerTransport};

const NET_QUEUE_SIZE: usize = 64;

type DeviceImpl<T> = VirtIONet<TwzHal, T, NET_QUEUE_SIZE>;

const NET_BUFFER_LEN: usize = 4096;

pub struct DeviceWrapper<T: Transport> {
    inner: Arc<Mutex<DeviceImpl<T>>>,
    rx_po: PacketObject,
    tx_po: PacketObject,
    tt_device: Arc<twizzler_driver::device::Device>,
}

impl<T: Transport> Clone for DeviceWrapper<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            rx_po: self.rx_po.clone(),
            tx_po: self.tx_po.clone(),
            tt_device: self.tt_device.clone(),
        }
    }
}

impl<T: Transport> DeviceWrapper<T> {
    fn new(
        dev: DeviceImpl<T>,
        rx_po: PacketObject,
        tt_device: Arc<twizzler_driver::device::Device>,
    ) -> Self {
        DeviceWrapper {
            inner: Arc::new(Mutex::new(dev)),
            rx_po,
            tx_po: PacketObject::new(ObjectCreate::default(), NET_QUEUE_SIZE, NET_BUFFER_LEN)
                .unwrap(),
            tt_device,
        }
    }

    pub fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.inner.lock().unwrap().mac_address())
    }

    pub fn has_work(&self) -> bool {
        self.tt_device.repr().check_for_interrupt(0).is_some()
    }

    pub fn get_sleep(&self) -> ThreadSync {
        ThreadSync::new_sleep(self.tt_device.repr().setup_interrupt_sleep(0))
    }

    pub fn get_rx(&self) -> Option<RxBuffer> {
        self.inner.lock().unwrap().receive().ok()
    }

    pub fn recycle(&self, rx: RxBuffer) {
        self.inner.lock().unwrap().recycle_rx_buffer(rx).unwrap();
    }

    pub fn transmit(&self, tx: TxBuffer) {
        self.inner.lock().unwrap().send(tx).unwrap();
    }
}

/*
impl<T: Transport> Device for DeviceWrapper<T> {
    type RxToken<'a>
        = VirtioRxToken<'a, T>
    where
        Self: 'a;
    type TxToken<'a>
        = VirtioTxToken<'a, T>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self.inner.lock().unwrap().receive() {
            Ok(buf) => Some((VirtioRxToken(self, buf), VirtioTxToken(self))),
            Err(Error::NotReady) => None,
            Err(err) => panic!("receive failed: {}", err),
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtioTxToken(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

pub struct VirtioRxToken<'a, T: Transport>(&'a DeviceWrapper<T>, RxBuffer);
pub struct VirtioTxToken<'a, T: Transport>(&'a DeviceWrapper<T>);

impl<'a, T: Transport> RxToken for VirtioRxToken<'a, T> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut rx_buf = self.1;
        let result = f(rx_buf.packet_mut());
        self.0
            .inner
            .lock()
            .unwrap()
            .recycle_rx_buffer(rx_buf)
            .unwrap();
        result
    }
}

impl<'a, T: Transport> TxToken for VirtioTxToken<'a, T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut tx_buf = TxBuffer::new(self.0.tx_po.clone(), len);
        let result = f(tx_buf.packet_mut());
        let mut dev = self.0.inner.lock().unwrap();
        dev.send(tx_buf).unwrap();
        result
    }
}

*/

// Gets the Virtio Net struct which implements the device used for smoltcp. Use this to create a
// smoltcp interface to send and receive packets. NOTE: Only the first device used will work
// properly
pub fn get_device() -> DeviceWrapper<TwizzlerTransport> {
    let rx_po = PacketObject::new(ObjectCreate::default(), NET_QUEUE_SIZE, NET_BUFFER_LEN).unwrap();
    let tt = TwizzlerTransport::new().unwrap();
    let tt_device = tt.device();
    let net = VirtIONet::<TwzHal, TwizzlerTransport, NET_QUEUE_SIZE>::new(
        tt,
        NET_BUFFER_LEN,
        rx_po.clone(),
    )
    .expect("failed to create net driver");
    DeviceWrapper::<TwizzlerTransport>::new(net, rx_po, tt_device)
}
