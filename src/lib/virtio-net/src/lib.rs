//! Virtio network device driver.
//!
//! Provides smoltcp types for use with the virtio network device.
mod hal;
mod tcp;
mod transport;

pub use tcp::{get_device, DeviceWrapper, VirtioRxToken, VirtioTxToken};
pub use transport::TwizzlerTransport;
