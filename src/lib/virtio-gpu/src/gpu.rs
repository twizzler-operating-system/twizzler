use std::sync::{Arc, Mutex};

use virtio_drivers::{
    device::gpu::VirtIOGpu,
    transport::{pci::VirtioPciError, Transport},
};

use crate::{hal::TwzHal, transport::TwizzlerTransport};

type DeviceImpl<T> = VirtIOGpu<TwzHal, T>;

#[derive(Clone)]
pub struct DeviceWrapper<T: Transport> {
    inner: Arc<Mutex<DeviceImpl<T>>>,
}

impl<T: Transport> DeviceWrapper<T> {
    fn new(dev: DeviceImpl<T>) -> Self {
        DeviceWrapper {
            inner: Arc::new(Mutex::new(dev)),
        }
    }

    pub fn with_device<R>(&self, f: impl FnOnce(&mut DeviceImpl<T>) -> R) -> R {
        f(&mut *self.inner.lock().unwrap())
    }
}

// Gets the Virtio Net struct which implements the device used for smoltcp. Use this to create a
// smoltcp interface to send and receive packets. NOTE: Only the first device used will work
// properly
pub fn get_device(
    notifier: std::sync::mpsc::Sender<Option<()>>,
) -> Result<DeviceWrapper<TwizzlerTransport>, VirtioPciError> {
    let gpu = VirtIOGpu::<TwzHal, TwizzlerTransport>::new(TwizzlerTransport::new(notifier)?)
        .expect("failed to create gpu driver");
    Ok(DeviceWrapper::<TwizzlerTransport>::new(gpu))
}
