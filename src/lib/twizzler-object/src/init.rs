use std::marker::PhantomData;

use twizzler_abi::{object::ObjID, syscall::ObjectMapError};

use crate::object::Object;
pub use twizzler_abi::object::Protections;

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
    /// Add an object with given id and protections to a new slot in the view.
    /// Populate object data with phantom data
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
