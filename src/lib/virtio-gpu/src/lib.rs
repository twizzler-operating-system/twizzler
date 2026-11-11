#![feature(iter_map_windows)]

//! Virtio network device driver.
//!
//! Provides smoltcp types for use with the virtio network device.
mod gpu;
mod hal;
mod input;
mod transport;

pub use gpu::{get_device, DeviceWrapper};
pub use input::{get_input_devices, InputDevice, InputEvent};
pub use transport::TwizzlerTransport;
