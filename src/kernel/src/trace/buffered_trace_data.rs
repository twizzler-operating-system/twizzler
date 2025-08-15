use alloc::boxed::Box;
use core::alloc::{GlobalAlloc, Layout};

use crate::memory::allocator::SLAB_ALLOCATOR;

const MAX_INLINE: usize = 16;
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

fn try_data_into_bytes<T: Copy, const MAX: usize>(data: &T) -> Option<[u8; MAX]> {
    let data = data as *const T as *const u8;
    let mut buf = [0; MAX];
    let len = size_of::<T>();
    if len > MAX {
        return None;
    }
    let data = unsafe { core::slice::from_raw_parts(data, len) };
    (&mut buf[0..len]).copy_from_slice(data);
    Some(buf)
}

impl BufferedTraceData {
    pub fn new<T: Copy>(data: T) -> Self {
        if let Some(bytes) = try_data_into_bytes(&data) {
            Self::Inline(bytes)
        } else {
            let b = Box::new(data);
            Self::Box(Box::into_raw(b).cast(), Layout::new::<T>())
        }
    }

    pub fn new_inline<T: Copy>(data: T) -> Option<Self> {
        try_data_into_bytes(&data).map(|b| Self::Inline(b))
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
