use std::sync::Arc;

use twizzler_abi::device::MailboxPriority;

use crate::device::{
    events::{DeviceEventStream, InterruptAllocationError, InterruptInfo},
    Device,
};

/// A single manager for both a device and an associated [DeviceEventStream].
pub struct DeviceController {
    device: Arc<Device>,
    events: Arc<DeviceEventStream>,
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
            events: Arc::new(DeviceEventStream::new(device)),
        }
    }

    /// Allocate a new interrupt on this device.
    pub fn allocate_interrupt(&self) -> Result<InterruptInfo, InterruptAllocationError> {
        self.events.allocate_interrupt()
    }

    /// Poll a single mailbox. If there are no messages, returns None.
    pub fn check_mailbox(&self, pri: MailboxPriority) -> Option<u64> {
        self.events.check_mailbox(pri)
    }

    /// Get the next message with a priority equal to or higher that `min`.
    pub async fn next_msg(&self, min: MailboxPriority) -> (MailboxPriority, u64) {
        self.events.next_msg(min).await
    }
}
