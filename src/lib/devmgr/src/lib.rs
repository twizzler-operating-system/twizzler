use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    marker::Invariant,
    object::Object,
};
use twizzler_rt_abi::{
    error::TwzError,
    object::{MapFlags, ObjID},
};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DriverSpec {
    pub supported: Supported,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum Supported {
    PcieClass(u8, u8, u8),
    Vendor(u16, u16),
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OwnedDevice {
    pub id: ObjID,
}

unsafe impl Invariant for OwnedDevice {}

#[secgate::gatecall]
pub fn devmgr_start() -> Result<(), TwzError> {}

#[secgate::gatecall]
pub fn get_devices(spec: DriverSpec) -> Result<ObjID, TwzError> {}

pub fn enumerate_devices(
    spec: DriverSpec,
) -> Result<VecObject<OwnedDevice, VecObjectAlloc>, TwzError> {
    let id = get_devices(spec)?;
    Ok(VecObject::from(Object::map(id, MapFlags::READ)?))
}
