//! Virtio network device driver.
//! 
//! Provides smoltcp types for use with the virtio network device.
mod tcp;
mod transport;
mod hal;

pub use tcp::{DeviceWrapper, VirtioRxToken, VirtioTxToken, get_device};