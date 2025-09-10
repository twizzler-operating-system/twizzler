use std::{
    alloc::{AllocError, GlobalAlloc},
    ptr::NonNull,
};

use super::talc::{LocalAllocator, LOCAL_ALLOCATOR};

pub struct TwzFerrocBase {
    pub local_alloc: &'static LocalAllocator,
}

impl TwzFerrocBase {
    pub const fn new() -> Self {
        Self {
            local_alloc: &LOCAL_ALLOCATOR,
        }
    }
}

unsafe impl ferroc::base::BaseAlloc for TwzFerrocBase {
    const IS_ZEROED: bool = false;

    type Handle = &'static LocalAllocator;

    type Error = AllocError;

    fn allocate(
        &self,
        layout: std::alloc::Layout,
        _commit: bool,
    ) -> Result<ferroc::base::Chunk<Self>, Self::Error> {
        let ptr = unsafe { self.local_alloc.alloc_zeroed(layout) };
        Ok(unsafe {
            Chunk::new(
                NonNull::new(ptr).ok_or(AllocError)?,
                layout,
                self.local_alloc,
            )
        })
    }

    unsafe fn deallocate(chunk: &mut ferroc::base::Chunk<Self>) {
        chunk
            .handle
            .dealloc(chunk.pointer().cast::<u8>().as_ptr(), chunk.layout());
    }
}

ferroc::config!(pub TwzFerroc => TwzFerrocBase);
