//! Traits and types for working with objects.

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use crate::{marker::BaseType, ptr::Ref};

mod builder;
mod fot;
mod meta;
mod mutable;
mod object;
mod tx;

pub use builder::*;
pub use fot::*;
pub use meta::*;
pub use mutable::MutObject;
pub use object::Object;
pub use twizzler_rt_abi::object::{MapFlags, ObjID, ObjectHandle};
pub use tx::TxObject;

/// Operations common to structured objects.
pub trait TypedObject {
    /// The base type of this object.
    type Base: BaseType;

    /// Returns a resolved reference to the object's base.
    fn base_ref(&self) -> Ref<'_, Self::Base>;

    fn base(&self) -> &Self::Base;
}

/// Operations common to all objects, with raw pointers.
pub trait RawObject {
    /// Get the underlying runtime handle for this object.
    fn handle(&self) -> &ObjectHandle;

    /// Get the object ID.
    fn id(&self) -> ObjID {
        self.handle().id()
    }

    /// Get a const pointer to the object base.
    fn base_ptr<T>(&self) -> *const T {
        self.lea(NULLPAGE_SIZE, size_of::<T>()).unwrap().cast()
    }

    /// Get a mut pointer to the object base.
    fn base_mut_ptr<T>(&self) -> *mut T {
        self.lea_mut(NULLPAGE_SIZE, size_of::<T>()).unwrap().cast()
    }

    /// Get a const pointer to the object metadata.
    fn meta_ptr(&self) -> *const MetaInfo {
        self.handle().meta().cast()
    }

    /// Get a mut pointer to the object metadata.
    fn meta_mut_ptr(&self) -> *mut MetaInfo {
        self.handle().meta().cast()
    }

    /// Get a const pointer to a given FOT entry.
    fn fote_ptr(&self, idx: usize) -> Option<*const FotEntry> {
        let offset: isize = (1 + idx).try_into().ok()?;
        unsafe { Some((self.meta_ptr() as *const FotEntry).offset(-offset)) }
    }

    /// Get a mut pointer to a given FOT entry.
    fn fote_ptr_mut(&self, idx: usize) -> Option<*mut FotEntry> {
        let offset: isize = (1 + idx).try_into().ok()?;
        unsafe { Some((self.meta_mut_ptr() as *mut FotEntry).offset(-offset)) }
    }

    /// Get a const pointer to given range of the object.
    fn lea(&self, offset: usize, _len: usize) -> Option<*const u8> {
        Some(unsafe { self.handle().start().add(offset) as *const u8 })
    }

    /// Get a mut pointer to given range of the object.
    fn lea_mut(&self, offset: usize, _len: usize) -> Option<*mut u8> {
        Some(unsafe { self.handle().start().add(offset) as *mut u8 })
    }

    /// If the pointer is local to this object, return the offset into the object. Otherwise, return
    /// None.
    fn ptr_local(&self, ptr: *const u8) -> Option<usize> {
        if ptr.addr() >= self.handle().start().addr()
            && ptr.addr() < self.handle().start().addr() + MAX_SIZE
        {
            Some(ptr.addr() - self.handle().start().addr())
        } else {
            None
        }
    }
}

impl RawObject for ObjectHandle {
    fn handle(&self) -> &ObjectHandle {
        self
    }
}
