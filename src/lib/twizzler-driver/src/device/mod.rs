use std::fmt::Display;
use std::sync::Mutex;

use bitvec::array::BitArray;

pub use twizzler_abi::device::BusType;
pub use twizzler_abi::device::DeviceRepr;
pub use twizzler_abi::device::DeviceType;
use twizzler_abi::device::NUM_DEVICE_INTERRUPTS;
use twizzler_abi::kso::KactionError;
use twizzler_abi::kso::KactionValue;
use twizzler_abi::kso::{KactionCmd, KactionFlags, KactionGenericCmd};
use twizzler_async::Async;
use twizzler_object::Object;
use twizzler_object::{ObjID, ObjectInitError, ObjectInitFlags, Protections};

use self::interrupts::InterruptData;
use self::interrupts::InterruptDataInner;

pub mod children;
pub mod events;
pub mod info;
pub mod interrupts;
pub mod mmio;

pub struct Device {
    obj: Object<DeviceRepr>,
    ints: [InterruptData; NUM_DEVICE_INTERRUPTS],
    taken_ints: Mutex<BitArray>,
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
        Ok(Self {
            obj,
            ints,
            taken_ints: Mutex::new(BitArray::ZERO),
        })
    }

    fn get_subobj(&self, ty: u8, idx: u8) -> Option<ObjID> {
        let cmd = KactionCmd::Generic(KactionGenericCmd::GetSubObject(ty, idx));
        let result =
            twizzler_abi::syscall::sys_kaction(cmd, Some(self.obj.id()), 0, KactionFlags::empty())
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
        twizzler_abi::syscall::sys_kaction(action, Some(self.obj.id()), value, flags)
    }
}
