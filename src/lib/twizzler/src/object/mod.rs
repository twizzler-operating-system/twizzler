use std::marker::PhantomData;

pub use twizzler_abi::object::ObjID;
pub use twizzler_abi::object::Protections;
use twizzler_abi::syscall::MapFlags;
use twizzler_abi::syscall::ObjectMapError;

bitflags::bitflags! {
    pub struct ObjectInitFlags: u32 {
    }
}

pub enum ObjectInitError {
    InvalidId,
    OutOfSlots,
    MappingFailed,
    InvalidProtections,
    ObjectNotFound,
}

impl From<ObjectMapError> for ObjectInitError {
    fn from(x: ObjectMapError) -> Self {
        match x {
            ObjectMapError::ObjectNotFound => ObjectInitError::ObjectNotFound,
            ObjectMapError::InvalidProtections => ObjectInitError::InvalidProtections,
            _ => ObjectInitError::MappingFailed,
        }
    }
}

pub struct Object<T> {
    slot: usize,
    id: ObjID,
    _pd: PhantomData<T>,
}

impl<T> Object<T> {
    pub fn base_raw(&self) -> &T {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot);
        unsafe { (start as *const T).as_ref().unwrap() }
    }

    pub fn init_id(
        id: ObjID,
        prot: Protections,
        _flags: ObjectInitFlags,
    ) -> Result<Self, ObjectInitError> {
        let slot = twizzler_abi::slot::global_allocate().ok_or(ObjectInitError::OutOfSlots)?;
        let _result =
            twizzler_abi::syscall::sys_object_map(None, id, slot, prot, MapFlags::empty())
                .map_err::<ObjectInitError, _>(|e| e.into())?;
        Ok(Self {
            slot,
            id,
            _pd: PhantomData,
        })
    }

    pub fn id(&self) -> ObjID {
        self.id
    }
}

impl<T> Drop for Object<T> {
    fn drop(&mut self) {
        twizzler_abi::slot::global_release(self.slot);
    }
}
