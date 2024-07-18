use std::marker::PhantomData;

use twizzler_runtime_api::ObjID;

use super::GlobalPtr;

pub struct InvPtrBuilder<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

impl<T> InvPtrBuilder<T> {
    /// Construct an invariant pointer builder from a global pointer.
    ///
    /// # Safety
    /// The caller must ensure that the lifetime of the pointed to data lives long enough.
    pub const unsafe fn from_global(gp: GlobalPtr<T>) -> Self {
        Self {
            id: gp.id(),
            offset: gp.offset(),
            _pd: PhantomData,
        }
    }

    /// Construct an invariant pointer from a local offset.
    ///
    /// # Safety
    /// The caller must ensure that the data in the local object referred to by this offset is valid
    /// and initialized, and of the correct type.
    pub const unsafe fn from_offset(offset: usize) -> Self {
        Self {
            id: ObjID::new(0),
            offset: offset as u64,
            _pd: PhantomData,
        }
    }

    pub const fn is_local(&self) -> bool {
        self.id.as_u128() == 0
    }

    pub const fn null() -> Self {
        Self {
            id: ObjID::new(0),
            offset: 0,
            _pd: PhantomData,
        }
    }

    pub const fn is_null(&self) -> bool {
        self.offset == 0
    }

    pub const fn id(&self) -> ObjID {
        self.id
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }
}
