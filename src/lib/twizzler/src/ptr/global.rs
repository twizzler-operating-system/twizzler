use std::marker::PhantomData;

use twizzler_runtime_api::{FotResolveError, ObjID};

use super::{ResolvedMutPtr, ResolvedPtr};

pub struct GlobalPtr<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

impl<T> GlobalPtr<T> {
    pub const fn new(id: ObjID, offset: u64) -> Self {
        Self {
            id,
            offset,
            _pd: PhantomData,
        }
    }

    pub fn from_va(ptr: *const T) -> Option<Self> {
        let runtime = twizzler_runtime_api::get_runtime();
        let (handle, offset) = runtime.ptr_to_handle(ptr as *const u8)?;
        Some(Self {
            id: handle.id,
            offset: offset as u64,
            _pd: PhantomData,
        })
    }

    pub const fn id(&self) -> ObjID {
        self.id
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn cast<U>(&self) -> GlobalPtr<U> {
        GlobalPtr::new(self.id, self.offset)
    }

    pub fn resolve(&self) -> Result<ResolvedPtr<'_, T>, FotResolveError> {
        todo!()
    }

    pub fn resolve_mut(&self) -> Result<ResolvedMutPtr<'_, T>, FotResolveError> {
        todo!()
    }
}

// Safety: These are the standard library rules for references (https://doc.rust-lang.org/std/primitive.reference.html).
unsafe impl<T: Sync> Sync for GlobalPtr<T> {}
unsafe impl<T: Sync> Send for GlobalPtr<T> {}
