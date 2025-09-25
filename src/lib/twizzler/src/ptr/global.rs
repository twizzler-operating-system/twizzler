use core::fmt::Debug;
use std::marker::PhantomData;

use twizzler_abi::object::ObjID;
use twizzler_rt_abi::object::{MapFlags, ObjectHandle};

use super::{Ref, RefMut};
use crate::{marker::Invariant, object::RawObject};

#[derive(Default, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// A global pointer, containing a fully qualified object ID and offset.
pub struct GlobalPtr<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

unsafe impl<T: Invariant> Invariant for GlobalPtr<T> {}

impl<T> Debug for GlobalPtr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalPtr")
            .field("id", &self.id())
            .field("offset", &self.offset())
            .finish()
    }
}

impl<T> GlobalPtr<T> {
    /// Creates a new global pointer.
    pub fn new(id: ObjID, offset: u64) -> Self {
        Self {
            id,
            offset,
            _pd: PhantomData,
        }
    }

    /// Casts the global pointer to a different type.
    pub fn cast<U>(self) -> GlobalPtr<U> {
        GlobalPtr::new(self.id, self.offset)
    }

    /// Checks if the global pointer is local from the perspective of a given object.
    pub fn is_local(&self, place: impl AsRef<ObjectHandle>) -> bool {
        place.as_ref().id() == self.id()
    }

    /// Resolve a global pointer into a reference.
    ///
    /// # Safety
    /// The underlying object must not mutate while the reference exists, unless
    /// the underlying type is Sync + Send. The memory referenced by the pointer
    /// must have an valid representation of the type.
    pub unsafe fn resolve_stable(&self) -> Ref<'_, T> {
        let handle = twizzler_rt_abi::object::twz_rt_map_object(
            self.id(),
            MapFlags::READ | MapFlags::INDIRECT,
        )
        .expect("failed to map global pointer object");
        let ptr = handle
            .lea(self.offset() as usize, size_of::<T>())
            .expect("failed to resolve global pointer");
        Ref::from_handle(handle, ptr.cast())
    }

    /// Resolve a global pointer into a reference.
    ///
    /// # Safety
    /// The underlying object must not mutate while the reference exists, unless
    /// the underlying type is Sync + Send. The memory referenced by the pointer
    /// must have an valid representation of the type.
    pub unsafe fn resolve(&self) -> Ref<'_, T> {
        let handle = twizzler_rt_abi::object::twz_rt_map_object(self.id(), MapFlags::READ)
            .expect("failed to map global pointer object");
        let ptr = handle
            .lea(self.offset() as usize, size_of::<T>())
            .expect("failed to resolve global pointer");
        Ref::from_handle(handle, ptr.cast())
    }

    /// Resolve a global pointer into a reference.
    ///
    /// # Safety
    /// The underlying object must not mutate while the reference exists, unless
    /// the underlying type is Sync + Send. The memory referenced by the pointer
    /// must have an valid representation of the type. No other references may be
    /// alive referring to the underlying data.
    pub unsafe fn resolve_mut(&self) -> RefMut<'_, T> {
        let handle = twizzler_rt_abi::object::twz_rt_map_object(
            self.id(),
            MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST,
        )
        .expect("failed to map global pointer object");
        let ptr = handle
            .lea_mut(self.offset() as usize, size_of::<T>())
            .expect("failed to resolve global pointer");
        RefMut::from_handle(handle, ptr.cast())
    }

    /// Returns true if the global pointer is null.
    pub fn is_null(&self) -> bool {
        self.id.raw() == 0
    }

    /// Returns the object ID of the global pointer.
    pub fn id(&self) -> ObjID {
        self.id
    }

    /// Returns the offset of the global pointer.
    pub fn offset(&self) -> u64 {
        self.offset
    }
}

impl<T> Clone for GlobalPtr<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            offset: self.offset,
            _pd: PhantomData,
        }
    }
}

impl<T> Copy for GlobalPtr<T> {}
