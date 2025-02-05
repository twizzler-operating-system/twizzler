//! Simple echo server over TCP.
//!
//! Ref: <https://github.com/smoltcp-rs/smoltcp/blob/master/examples/server.rs>
use std::sync::{Arc, Mutex};

use smoltcp::{
    iface::SocketHandle,
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    time::Instant,
    wire::EthernetAddress,
};
use virtio_drivers::{
    device::net::{RxBuffer, VirtIONet},
    transport::Transport,
    Error,
};

use crate::{hal::TestHal, transport::TwizzlerTransport};

const NET_QUEUE_SIZE: usize = 16;

type DeviceImpl<T> = VirtIONet<TestHal, T, NET_QUEUE_SIZE>;

const NET_BUFFER_LEN: usize = 2048;

pub struct DeviceWrapper<T: Transport> {
    inner: Arc<Mutex<DeviceImpl<T>>>,
}

impl<T: Transport> DeviceWrapper<T> {
    fn new(dev: DeviceImpl<T>) -> Self {
        DeviceWrapper {
            inner: Arc::new(Mutex::new(dev)),
        }
    }

    pub fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.inner.lock().unwrap().mac_address())
    }
}

impl<T: Transport> Device for DeviceWrapper<T> {
    type RxToken<'a>
        = VirtioRxToken<T>
    where
        Self: 'a;
    type TxToken<'a>
        = VirtioTxToken<T>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self.inner.lock().unwrap().receive() {
            Ok(buf) => Some((
                VirtioRxToken(self.inner.clone(), buf),
                VirtioTxToken(self.inner.clone()),
            )),
            Err(Error::NotReady) => None,
            Err(err) => panic!("receive failed: {}", err),
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtioTxToken(self.inner.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1536;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

pub struct VirtioRxToken<T: Transport>(Arc<Mutex<DeviceImpl<T>>>, RxBuffer);
pub struct VirtioTxToken<T: Transport>(Arc<Mutex<DeviceImpl<T>>>);

impl<T: Transport> RxToken for VirtioRxToken<T> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut rx_buf = self.1;
        let result = f(rx_buf.packet_mut());
        self.0.lock().unwrap().recycle_rx_buffer(rx_buf).unwrap();
        result
    }
}

impl<T: Transport> TxToken for VirtioTxToken<T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut dev = self.0.lock().unwrap();
        let mut tx_buf = dev.new_tx_buffer(len);
        let result = f(tx_buf.packet_mut());
        dev.send(tx_buf).unwrap();
        result
    }
}

// Gets the Virtio Net struct which implements the device used for smoltcp. Use this to create a
// smoltcp interface to send and receive packets. NOTE: Only the first device used will work
// properly
pub fn get_device(
    notifier: std::sync::mpsc::Sender<Option<(SocketHandle, u16)>>,
) -> DeviceWrapper<TwizzlerTransport> {
    let net = VirtIONet::<TestHal, TwizzlerTransport, NET_QUEUE_SIZE>::new(
        TwizzlerTransport::new(notifier).unwrap(),
        NET_BUFFER_LEN,
    )
    .expect("failed to create net driver");
    DeviceWrapper::<TwizzlerTransport>::new(net)
}
