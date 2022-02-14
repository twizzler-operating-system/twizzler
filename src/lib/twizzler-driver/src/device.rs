use std::{fmt::Display, marker::PhantomData};

use twizzler::object::{ObjID, ObjectInitError, ObjectInitFlags, Protections};
pub use twizzler_abi::device::DeviceRepr;
use twizzler_abi::{
    device::SubObjectType,
    kso::{KactionCmd, KactionFlags, KactionGenericCmd},
};

pub struct Device {
    obj: twizzler::object::Object<DeviceRepr>,
}

impl Display for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repr = self.repr();
        repr.fmt(f)
    }
}

pub struct InfoObject<T> {
    id: ObjID,
    _pd: PhantomData<T>,
}
pub struct MmioObject {
    id: ObjID,
}

impl MmioObject {
    fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        Ok(Self { id })
    }
}

impl<T> InfoObject<T> {
    fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        Ok(Self {
            _pd: PhantomData,
            id,
        })
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
            obj: twizzler::object::Object::init_id(
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

    pub unsafe fn get_info<T>(&self, idx: u8) -> Option<InfoObject<T>> {
        let id = self.get_subobj(SubObjectType::Mmio.into(), idx)?;
        InfoObject::new(id).ok()
    }

    pub fn children(&self) -> DeviceChildrenIterator {
        DeviceChildrenIterator {
            id: self.obj.id(),
            pos: 0,
        }
    }

    pub fn repr(&self) -> &DeviceRepr {
        self.obj.base_raw()
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
