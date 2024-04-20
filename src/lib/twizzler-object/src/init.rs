use std::marker::PhantomData;

pub use twizzler_abi::object::Protections;
use twizzler_abi::{object::ObjID, syscall::ObjectMapError};

use crate::object::Object;

bitflags::bitflags! {
    /// Flags to pass to object initialization routines.
    pub struct ObjectInitFlags: u32 {
    }
}

/// Possible errors from initializing an object handle.
#[derive(Debug, Copy, Clone)]
pub enum ObjectInitError {
    /// The ID isn't valid.
    InvalidId,
    /// There are not enough memory slots.
    OutOfSlots,
    /// The mapping failed.
    MappingFailed,
    /// The requested protections are invalid.
    InvalidProtections,
    /// The object doesn't exist.
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
    /// Initialize an object handle from an object ID.
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
