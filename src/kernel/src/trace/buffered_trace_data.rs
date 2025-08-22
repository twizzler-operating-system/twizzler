use alloc::sync::Arc;

const MAX_INLINE: usize = 128;
const MAX_BOX: usize = 1024;
#[derive(Clone, Debug)]
pub enum BufferedTraceData {
    Box(Arc<[u8; MAX_BOX]>, usize),
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
            let b = Arc::new(
                // If this fails, just increase the memory, or don't try to store so much data in a
                // trace event.
                try_data_into_bytes(&data)
                    .expect("failed to allocate enough memory for trace event"),
            );
            Self::Box(b, size_of::<T>().next_multiple_of(MAX_INLINE))
        }
    }

    pub fn new_inline<T: Copy>(data: T) -> Option<Self> {
        try_data_into_bytes(&data).map(|b| Self::Inline(b))
    }

    pub fn len(&self) -> usize {
        match self {
            BufferedTraceData::Box(_, size) => *size,
            BufferedTraceData::Inline(bytes) => bytes.len(),
        }
    }

    pub fn ptr(&self) -> *const u8 {
        match self {
            BufferedTraceData::Box(ptr, _) => {
                let x = &*(*ptr);
                x.as_ptr()
            }
            BufferedTraceData::Inline(bytes) => bytes.as_ptr(),
        }
    }
}
