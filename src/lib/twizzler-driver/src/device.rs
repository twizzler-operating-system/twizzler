use std::fmt::Display;

use futures::stream::select_all;

use futures::FutureExt;
use futures::Stream;
pub use twizzler_abi::device::BusType;
pub use twizzler_abi::device::DeviceRepr;
pub use twizzler_abi::device::DeviceType;
use twizzler_abi::device::MmioInfo;
use twizzler_abi::device::MMIO_OFFSET;
use twizzler_abi::device::NUM_DEVICE_INTERRUPTS;
use twizzler_abi::kso::KactionError;
use twizzler_abi::kso::KactionValue;
use twizzler_abi::marker::BaseType;
use twizzler_abi::marker::ObjSafe;
use twizzler_abi::{
    device::SubObjectType,
    kso::{KactionCmd, KactionFlags, KactionGenericCmd},
};
use twizzler_async::Async;
use twizzler_async::AsyncSetup;
use twizzler_object::Object;
use twizzler_object::{ObjID, ObjectInitError, ObjectInitFlags, Protections};

#[derive(Debug)]
struct InterruptDataInner {
    repr: *const DeviceRepr,
    inum: usize,
}

impl AsyncSetup for InterruptDataInner {
    type Error = bool;

    const WOULD_BLOCK: Self::Error = true;

    fn setup_sleep(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let repr = unsafe { self.repr.as_ref().unwrap_unchecked() };
        repr.setup_interrupt_sleep(self.inum)
    }
}

#[derive(Debug)]
struct InterruptData {
    src: Async<InterruptDataInner>,
}

pub struct Device {
    obj: Object<DeviceRepr>,
    ints: [InterruptData; NUM_DEVICE_INTERRUPTS],
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
        self.obj.base().unwrap()
    }

    /// Get the base of the memory mapped IO region.
    /// # Safety
    /// The type this returns is not verified in any way, so the caller must ensure that T is
    /// the correct type for the underlying data.
    pub unsafe fn get_mmio_offset<T>(&self, offset: usize) -> &T {
        let ptr = self.obj.base().unwrap() as *const MmioInfo as *const u8;
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
        self.obj.base().unwrap()
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
        let obj = Object::init_id(
            id,
            Protections::WRITE | Protections::READ,
            ObjectInitFlags::empty(),
        )?;
        let ints = (0..NUM_DEVICE_INTERRUPTS)
            .into_iter()
            .map(|i| InterruptData {
                src: Async::new(InterruptDataInner {
                    repr: obj.base().unwrap() as *const DeviceRepr,
                    inum: i,
                }),
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        Ok(Self { obj, ints })
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
        self.obj.base().unwrap()
    }

    pub fn is_bus(&self) -> bool {
        let repr = self.repr();
        repr.device_type == DeviceType::Bus
    }

    pub fn bus_type(&self) -> BusType {
        self.repr().bus_type
    }

    pub fn next_interrupt(
        &self,
        inum: usize,
    ) -> impl Stream<Item = Result<(usize, u64), bool>> + '_ {
        let repr = self.repr();
        self.ints[inum]
            .src
            .run_with(move |inner| {
                repr.check_for_interrupt(inum)
                    .ok_or(true)
                    .map(|x| (inner.inum, x))
            })
            .into_stream()
    }

    pub fn next_any_interrupt(&self) -> impl Stream<Item = Result<(usize, u64), bool>> + '_ {
        select_all(
            self.ints
                .iter()
                .map(|i| Box::pin(self.next_interrupt(i.src.get_ref().inum))),
        )
    }

    pub fn kaction(
        &self,
        action: KactionCmd,
        value: u64,
        flags: KactionFlags,
    ) -> Result<KactionValue, KactionError> {
        twizzler_abi::syscall::sys_kaction(action, Some(self.obj.id()), value, flags)
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
