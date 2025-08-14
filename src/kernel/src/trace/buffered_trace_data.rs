use alloc::boxed::Box;
use core::alloc::{GlobalAlloc, Layout};

use twizzler_abi::trace::TraceData;

use crate::memory::allocator::SLAB_ALLOCATOR;

const MAX_INLINE: usize = 32;
#[derive(Clone, Copy, Debug)]
pub enum BufferedTraceData {
    Box(*mut u8, Layout),
    Inline([u8; MAX_INLINE]),
}

impl Default for BufferedTraceData {
    fn default() -> Self {
        Self::Inline([0; MAX_INLINE])
    }
}

impl BufferedTraceData {
    pub fn new<T: Copy>(td: TraceData<T>) -> Self {
        if let Some(bytes) = td.try_into_bytes() {
            Self::Inline(bytes)
        } else {
            let b = Box::new(td);
            Self::Box(Box::into_raw(b).cast(), Layout::new::<T>())
        }
    }

    pub fn new_inline<T: Copy>(td: TraceData<T>) -> Option<Self> {
        td.try_into_bytes().map(|b| Self::Inline(b))
    }

    pub fn free(&mut self) {
        match self {
            BufferedTraceData::Box(ptr, layout) => unsafe { SLAB_ALLOCATOR.dealloc(*ptr, *layout) },
            _ => {}
        }
    }

    pub fn len(&self) -> usize {
        match self {
            BufferedTraceData::Box(_, layout) => layout.size(),
            BufferedTraceData::Inline(bytes) => bytes.len(),
        }
    }

    pub fn ptr(&self) -> *const u8 {
        match self {
            BufferedTraceData::Box(ptr, _) => *ptr,
            BufferedTraceData::Inline(bytes) => bytes.as_ptr(),
        }
    }
}

unsafe impl Send for BufferedTraceData {}
unsafe impl Sync for BufferedTraceData {}
