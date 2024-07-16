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

    pub const fn id(&self) -> ObjID {
        self.id
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }
}
