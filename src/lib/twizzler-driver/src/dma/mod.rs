//! Module for managing DMA memory, using objects for DMA, and creating pools of DMA memory that can
//! be allocated from. Internally, the DMA functions will interact with the kernel to ensure
//! stability of physical addresses for DMA memory, and will also ensure proper coherence between
//! the host and devices.

mod object;
mod pin;
mod pool;
mod region;

use std::cell::UnsafeCell;

pub use object::DmaObject;
pub use pin::{DmaPin, PhysAddr, PhysInfo, PinError};
pub use pool::DmaPool;
pub use region::{DmaRegion, DmaSliceRegion};

pub use super::arch::DMA_PAGE_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Intended access direction for DMA memory.
pub enum Access {
    /// The memory is used for the host to write and the device to read. Device writes may not be
    /// coherent.
    HostToDevice,
    /// The memory is used for the host to read and the device to write. Host writes may not be
    /// coherent.
    DeviceToHost,
    /// The memory is accessed read/write by both device and host.
    BiDirectional,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Mode of syncing to apply when calling `sync()`. These sync calls are unnecessary by default, and
/// should only be used with utmost care.
pub enum SyncMode {
    /// Ensures coherence for the host to write to the device, ensuring that the memory is coherent
    /// from the perspective of the CPU before the host writes.
    PreCpuToDevice,
    /// Ensures coherence for the host to write to the device, ensuring that the memory is coherent
    /// after the write.
    PostCpuToDevice,
    /// Ensures coherence for the device to write to the host, ensuring that the memory is coherent
    /// before the device performs an operation.
    PreDeviceToCpu,
    /// Ensures coherence for the device to write to the host, ensuring that the memory is coherent
    /// after the device performs an operation.
    PostDeviceToCpu,
    /// Ensures that memory is fully coherent.
    FullCoherence,
}

bitflags::bitflags! {
    /// Options for DMA regions.
    #[derive(Clone, Copy, Debug)]
    pub struct DmaOptions : u64 {
        /// Region functions will not perform automatic coherence.
        const UNSAFE_MANUAL_COHERENCE = 1;
    }
}

impl Default for DmaOptions {
    fn default() -> Self {
        Self::empty()
    }
}

/// DMA types must implement this trait, which indicates that types can handle untyped updates from
/// the device.
pub auto trait DeviceSync {}

impl DeviceSync for u8 {}
impl DeviceSync for u16 {}
impl DeviceSync for u32 {}
impl DeviceSync for u64 {}
impl DeviceSync for i8 {}
impl DeviceSync for i16 {}
impl DeviceSync for i32 {}
impl DeviceSync for i64 {}

impl<T> !DeviceSync for *const T {}
impl<T> !DeviceSync for *mut T {}
impl<T> !DeviceSync for &T {}
impl<T> !DeviceSync for &mut T {}
impl<T> !DeviceSync for UnsafeCell<T> {}
impl<T> !DeviceSync for std::cell::Cell<T> {}
