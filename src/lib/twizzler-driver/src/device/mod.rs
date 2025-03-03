//! Functions and types for managing a device.

use std::fmt::Display;

use twizzler::object::{ObjID, Object, RawObject};
pub use twizzler_abi::device::{BusType, DeviceRepr, DeviceType};
use twizzler_abi::kso::{KactionCmd, KactionError, KactionFlags, KactionGenericCmd, KactionValue};

mod children;
pub mod events;
mod info;
mod mmio;

pub use children::DeviceChildrenIterator;
pub use info::InfoObject;
pub use mmio::MmioObject;
use twizzler_rt_abi::object::{MapError, MapFlags};

/// A handle for a device.
pub struct Device {
    obj: Object<DeviceRepr>,
}

impl Display for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repr = self.repr();
        repr.fmt(f)
    }
}

impl Device {
    pub fn new(id: ObjID) -> Result<Self, MapError> {
        let obj = unsafe { Object::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?;

        Ok(Self { obj })
    }

    fn get_subobj(&self, ty: u8, idx: u8) -> Option<ObjID> {
        let cmd = KactionCmd::Generic(KactionGenericCmd::GetSubObject(ty, idx));
        let result = twizzler_abi::syscall::sys_kaction(
            cmd,
            Some(self.obj.id()),
            0,
            0,
            KactionFlags::empty(),
        )
        .ok()?;
        result.objid()
    }

    /// Get a reference to a device's representation data.
    pub fn repr(&self) -> &DeviceRepr {
        unsafe { self.obj.base_ptr::<DeviceRepr>().as_ref().unwrap() }
    }

    /// Get a mutable reference to a device's representation data.
    pub fn repr_mut(&self) -> &mut DeviceRepr {
        unsafe { self.obj.base_mut_ptr::<DeviceRepr>().as_mut().unwrap() }
    }

    /// Is this device a bus?
    pub fn is_bus(&self) -> bool {
        let repr = self.repr();
        repr.device_type == DeviceType::Bus
    }

    /// Get the bus type of this device.
    pub fn bus_type(&self) -> BusType {
        self.repr().bus_type
    }

    /// Execute a kaction operation on a device.
    pub fn kaction(
        &self,
        action: KactionCmd,
        value: u64,
        flags: KactionFlags,
        value2: u64,
    ) -> Result<KactionValue, KactionError> {
        twizzler_abi::syscall::sys_kaction(action, Some(self.obj.id()), value, value2, flags)
    }

    pub fn id(&self) -> ObjID {
        self.obj.id()
    }
}
