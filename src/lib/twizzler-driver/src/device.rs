use std::fmt::Display;

pub use twizzler_abi::device::BusType;
pub use twizzler_abi::device::DeviceRepr;
pub use twizzler_abi::device::DeviceType;
use twizzler_abi::device::MmioInfo;
use twizzler_abi::device::MMIO_OFFSET;
use twizzler_abi::marker::BaseType;
use twizzler_abi::marker::ObjSafe;
use twizzler_abi::{
    device::SubObjectType,
    kso::{KactionCmd, KactionFlags, KactionGenericCmd},
};
use twizzler_object::Object;
use twizzler_object::{ObjID, ObjectInitError, ObjectInitFlags, Protections};

pub struct Device {
    obj: Object<DeviceRepr>,
}

impl Display for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repr = self.repr();
        repr.fmt(f)
    }
}

pub struct InfoObject<T> {
    obj: Object<T>,
}
pub struct MmioObject {
    obj: Object<MmioInfo>,
}

impl MmioObject {
    fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        Ok(Self {
            obj: Object::init_id(
                id,
                Protections::READ | Protections::WRITE,
                ObjectInitFlags::empty(),
            )?,
        })
    }

    // TODO: no unwrap
    pub fn get_info(&self) -> &MmioInfo {
        self.obj.base_notx().unwrap()
    }

    /// Get the base of the memory mapped IO region.
    /// # Safety
    /// The type this returns is not verified in any way, so the caller must ensure that T is
    /// the correct type for the underlying data.
    pub unsafe fn get_mmio_offset<T>(&self, offset: usize) -> &T {
        let ptr = self.obj.base_notx().unwrap() as *const MmioInfo as *const u8;
        (ptr.add(MMIO_OFFSET + offset).sub(0x1000) as *mut T)
            .as_mut()
            .unwrap()
    }
}

impl<T: BaseType + ObjSafe> InfoObject<T> {
    fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        Ok(Self {
            obj: Object::init_id(id, Protections::READ, ObjectInitFlags::empty())?,
        })
    }

    pub fn get_data(&self) -> &T {
        self.obj.base_notx().unwrap()
    }
}

pub struct DeviceChildrenIterator {
    id: ObjID,
    pos: u16,
}

impl Iterator for DeviceChildrenIterator {
    type Item = Device;
    fn next(&mut self) -> Option<Self::Item> {
        let cmd = KactionCmd::Generic(KactionGenericCmd::GetChild(self.pos));
        let result =
            twizzler_abi::syscall::sys_kaction(cmd, Some(self.id), 0, KactionFlags::empty())
                .ok()?;
        self.pos += 1;
        result.objid().map(|id| Device::new(id).ok()).flatten()
    }
}

impl Device {
    fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        Ok(Self {
            obj: Object::init_id(
                id,
                Protections::WRITE | Protections::READ,
                ObjectInitFlags::empty(),
            )?,
        })
    }

    fn get_subobj(&self, ty: u8, idx: u8) -> Option<ObjID> {
        let cmd = KactionCmd::Generic(KactionGenericCmd::GetSubObject(ty, idx));
        let result =
            twizzler_abi::syscall::sys_kaction(cmd, Some(self.obj.id()), 0, KactionFlags::empty())
                .ok()?;
        result.objid()
    }

    pub fn get_mmio(&self, idx: u8) -> Option<MmioObject> {
        let id = self.get_subobj(SubObjectType::Mmio.into(), idx)?;
        MmioObject::new(id).ok()
    }

    /// Get an indexed info object for a device.
    /// # Safety
    /// The type T is not verified in any way, so the caller must ensure that T is correct
    /// for the underlying data.
    pub unsafe fn get_info<T: ObjSafe + BaseType>(&self, idx: u8) -> Option<InfoObject<T>> {
        let id = self.get_subobj(SubObjectType::Info.into(), idx)?;
        InfoObject::new(id).ok()
    }

    pub fn children(&self) -> DeviceChildrenIterator {
        DeviceChildrenIterator {
            id: self.obj.id(),
            pos: 0,
        }
    }

    pub fn repr(&self) -> &DeviceRepr {
        self.obj.base_notx().unwrap()
    }

    pub fn is_bus(&self) -> bool {
        let repr = self.repr();
        repr.device_type == DeviceType::Bus
    }

    pub fn bus_type(&self) -> BusType {
        self.repr().bus_type
    }
}

pub struct BusTreeRoot {
    root_id: ObjID,
}

impl BusTreeRoot {
    pub fn children(&self) -> DeviceChildrenIterator {
        DeviceChildrenIterator {
            id: self.root_id,
            pos: 0,
        }
    }
}

pub fn get_bustree_root() -> BusTreeRoot {
    let cmd = KactionCmd::Generic(KactionGenericCmd::GetKsoRoot);
    let id = twizzler_abi::syscall::sys_kaction(cmd, None, 0, KactionFlags::empty())
        .expect("failed to get device root")
        .unwrap_objid();
    BusTreeRoot { root_id: id }
}
