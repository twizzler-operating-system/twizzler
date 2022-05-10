use std::marker::PhantomData;

use twizzler_abi::{object::{Protections, ObjID}, syscall::{MapFlags, ObjectMapError}};

use crate::object::Object;

bitflags::bitflags! {
    pub struct ObjectInitFlags: u32 {
    }
}

#[derive(Debug, Copy, Clone)]
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

impl<T> Object<T> {
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
}

