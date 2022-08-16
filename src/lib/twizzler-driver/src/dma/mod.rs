mod object;
mod pin;
mod pool;
mod region;

pub use object::DmaObject;
pub use pin::DmaPin;
pub use pool::DmaPool;
pub use region::{DmaArrayRegion, DmaRegion};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum Access {
    HostToDevice,
    DeviceToHost,
    BiDirectional,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum SyncMode {
    PreCpuToDevice,
    PostCpuToDevice,
    PreDeviceToCpu,
    PostDeviceToCpu,
}

bitflags::bitflags! {
    pub struct DmaOptions : u64 {
        const UNSAFE_MANUAL_COHERENCE = 1;
    }
}

impl Default for DmaOptions {
    fn default() -> Self {
        Self::empty()
    }
}

pub trait DeviceSync {}

impl DeviceSync for u8 {}
impl DeviceSync for u16 {}
impl DeviceSync for u32 {}
impl DeviceSync for u64 {}
impl DeviceSync for i8 {}
impl DeviceSync for i16 {}
impl DeviceSync for i32 {}
impl DeviceSync for i64 {}
