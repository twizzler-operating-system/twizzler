use std::fmt::Display;

pub use twizzler_abi::device::BusType;
pub use twizzler_abi::device::DeviceRepr;
pub use twizzler_abi::device::DeviceType;
use twizzler_abi::kso::KactionError;
use twizzler_abi::kso::KactionValue;
use twizzler_abi::kso::{KactionCmd, KactionFlags, KactionGenericCmd};
use twizzler_object::Object;
use twizzler_object::{ObjID, ObjectInitError, ObjectInitFlags, Protections};

pub mod children;
pub mod events;
pub mod info;
pub mod mmio;

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

    pub fn repr(&self) -> &DeviceRepr {
        self.obj.base().unwrap()
    }

    pub fn repr_mut(&self) -> &mut DeviceRepr {
        unsafe { self.obj.base_mut_unchecked() }
    }

    pub fn is_bus(&self) -> bool {
        let repr = self.repr();
        repr.device_type == DeviceType::Bus
    }

    pub fn bus_type(&self) -> BusType {
        self.repr().bus_type
    }

    pub fn kaction(
        &self,
        action: KactionCmd,
        value: u64,
        flags: KactionFlags,
    ) -> Result<KactionValue, KactionError> {
        twizzler_abi::syscall::sys_kaction(action, Some(self.obj.id()), value, 0, flags)
    }
}
