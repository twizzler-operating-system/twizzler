use std::sync::Arc;

use crate::device::{events::DeviceEventStream, Device};

/// A single manager for both a device and an associated [DeviceEventStream].
pub struct DeviceController {
    device: Arc<Device>,
    events: DeviceEventStream,
}

impl DeviceController {
    /// Get a reference to the event stream.
    pub fn events(&self) -> &DeviceEventStream {
        &self.events
    }

    /// Get a reference to the device.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Create a new device controller from a device.
    pub fn new_from_device(device: Device) -> Self {
        let device = Arc::new(device);
        Self {
            device: device.clone(),
            events: DeviceEventStream::new(device),
        }
    }
}
