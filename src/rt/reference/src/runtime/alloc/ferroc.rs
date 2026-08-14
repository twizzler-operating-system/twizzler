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
        let ptr = unsafe { self.local_alloc.alloc(layout) };
        // ferroc finds a block's owning slab by masking to SLAB_SIZE (slab.rs:134), and only
        // checks that we honored the requested alignment under `debug_assert!` (arena.rs:123),
        // which is compiled out in release. Verify it on our side of the boundary. Logging rather
        // than asserting: a panic here would re-enter the allocator to format its message.
        if !ptr.is_null() && (ptr as usize) % layout.align() != 0 {
            twizzler_abi::klog_println!(
                "FERROC-BASE-MISALIGN: ptr {:p} size {:x} align {:x}",
                ptr,
                layout.size(),
                layout.align()
            );
        }
        Ok(unsafe {
            ferroc::base::Chunk::new(
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

    unsafe fn commit(&self, ptr: NonNull<[u8]>) -> Result<(), Self::Error> {
        let _ = ptr;
        Ok(())
    }

    unsafe fn decommit(&self, ptr: NonNull<[u8]>) {
        let _ = ptr;
    }
}

ferroc::config!(pub TwzFerroc => TwzFerrocBase: pthread);
