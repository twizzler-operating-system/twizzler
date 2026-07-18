//! VirtIO input device driver for Twizzler.
//!
//! Wraps the virtio-drivers VirtIOInput device with Twizzler transport and DMA,
//! providing a simple event polling API.

mod hal;
mod transport;

use std::sync::{Arc, Mutex};

use virtio_drivers::{
    device::input::{InputEvent, VirtIOInput},
    transport::{pci::VirtioPciError, Transport},
};

use crate::{hal::TwzHal, transport::TwizzlerTransport};

pub use transport::TwizzlerTransport as InputTransport;
pub use virtio_drivers::device::input::InputEvent as VirtioInputEvent;

type DeviceImpl<T> = VirtIOInput<TwzHal, T>;

#[derive(Clone)]
pub struct InputDevice<T: Transport> {
    inner: Arc<Mutex<DeviceImpl<T>>>,
}

impl<T: Transport> InputDevice<T> {
    fn new(dev: DeviceImpl<T>) -> Self {
        InputDevice {
            inner: Arc::new(Mutex::new(dev)),
        }
    }

    pub fn pop_event(&self) -> Option<InputEvent> {
        self.inner.lock().unwrap().pop_pending_event()
    }

    pub fn with_device<R>(&self, f: impl FnOnce(&mut DeviceImpl<T>) -> R) -> R {
        f(&mut *self.inner.lock().unwrap())
    }
}

pub fn get_device(
    notifier: std::sync::mpsc::Sender<Option<()>>,
) -> Result<InputDevice<TwizzlerTransport>, VirtioPciError> {
    let input =
        VirtIOInput::<TwzHal, TwizzlerTransport>::new(TwizzlerTransport::new(notifier)?)
            .expect("failed to create input driver");
    Ok(InputDevice::<TwizzlerTransport>::new(input))
}
