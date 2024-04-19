//! Functions and types for managing a device.

use std::fmt::Display;

pub use twizzler_abi::device::{BusType, DeviceRepr, DeviceType};
use twizzler_abi::kso::{KactionCmd, KactionError, KactionFlags, KactionGenericCmd, KactionValue};
use twizzler_object::{ObjID, Object, ObjectInitError, ObjectInitFlags, Protections};

mod children;
pub mod events;
mod info;
mod mmio;

pub use children::DeviceChildrenIterator;
pub use info::InfoObject;
pub use mmio::MmioObject;

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
    fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        let obj = Object::init_id(
            id,
            Protections::WRITE | Protections::READ,
            ObjectInitFlags::empty(),
        )?;

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
        self.obj.base().unwrap()
    }

    /// Get a mutable reference to a device's representation data.
    pub fn repr_mut(&self) -> &mut DeviceRepr {
        unsafe { self.obj.base_mut_unchecked() }
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
}
