use twizzler_driver::device::Device;
pub use virtio_drivers::device::input::InputEvent;
use virtio_drivers::device::input::VirtIOInput;

use crate::{hal::TwzHal, transport::TwizzlerTransport};

pub type InputDevice = VirtIOInput<TwzHal, TwizzlerTransport>;

const VIRTIO_VENDOR: u16 = 0x1af4;
const VIRTIO_INPUT: u16 = 0x1052;

/// Every virtio-input device (keyboard, mouse, tablet). `notifier` is sent to on an interrupt
/// from any of them.
pub fn get_input_devices(notifier: std::sync::mpsc::Sender<Option<()>>) -> Vec<InputDevice> {
    let Ok(devices) = devmgr::enumerate_devices(devmgr::DriverSpec {
        supported: devmgr::Supported::Vendor(VIRTIO_VENDOR, VIRTIO_INPUT),
    }) else {
        return Vec::new();
    };
    let mut inputs = Vec::new();
    for device in &devices {
        let Ok(device) = Device::new(device.id) else {
            continue;
        };
        let Ok(transport) = TwizzlerTransport::for_device(device, notifier.clone()) else {
            continue;
        };
        match VirtIOInput::new(transport) {
            Ok(input) => inputs.push(input),
            Err(e) => tracing::warn!("virtio-input device failed to start: {:?}", e),
        }
    }
    inputs
}
