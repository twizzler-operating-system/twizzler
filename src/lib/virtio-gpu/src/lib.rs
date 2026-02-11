#![feature(iter_map_windows)]

//! Virtio network device driver.
//!
//! Provides smoltcp types for use with the virtio network device.
mod gpu;
mod hal;
mod transport;

pub use gpu::{get_device, DeviceWrapper};
pub use transport::TwizzlerTransport;
