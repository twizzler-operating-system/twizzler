use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    marker::Invariant,
    object::Object,
};
use twizzler_rt_abi::object::{MapFlags, ObjID};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DriverSpec {
    pub supported: Supported,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum Supported {
    PcieClass(u8, u8, u8),
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OwnedDevice {
    pub id: ObjID,
}

unsafe impl Invariant for OwnedDevice {}

pub fn get_devices(spec: DriverSpec) -> Option<VecObject<OwnedDevice, VecObjectAlloc>> {
    let devcomp = monitor_api::CompartmentHandle::lookup("devmgr")?;
    let get_devices = unsafe {
        devcomp
            .dynamic_gate::<(DriverSpec,), Option<ObjID>>("get_devices")
            .unwrap()
    };
    let id = (get_devices)(spec).ok().flatten()?;
    Some(VecObject::from(Object::map(id, MapFlags::READ).ok()?))
}
