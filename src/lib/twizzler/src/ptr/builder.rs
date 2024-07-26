use std::{marker::PhantomData, sync::atomic::AtomicU32};

use twizzler_runtime_api::ObjID;

use super::{GlobalPtr, InvPtr};
use crate::object::fot::FotEntry;

pub struct InvPtrBuilder<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

impl<T> InvPtrBuilder<T> {
    /// Construct an invariant pointer builder from a global pointer.
    pub const fn from_global(gp: GlobalPtr<T>) -> Self {
        Self {
            id: gp.id(),
            offset: gp.offset(),
            _pd: PhantomData,
        }
    }

    /// Construct an invariant pointer from a local offset.
    pub const fn from_offset(offset: usize) -> Self {
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

    pub fn fot_entry(&self) -> FotEntry {
        FotEntry {
            values: [self.id().split().0, self.id().split().1],
            resolver: InvPtr::null(),
            flags: AtomicU32::new(0),
        }
    }
}
