use std::marker::PhantomData;

use twizzler_abi::{
    object::ObjID,
    syscall::{ObjectMapError},
};

use crate::object::Object;
pub use twizzler_abi::object::Protections;

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
        Ok(Self {
            slot: crate::slot::get(id, prot)?,
            _pd: PhantomData,
        })
    }
}
