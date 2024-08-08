use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_runtime_api::ObjectHandle;

/// A simple buffer to use for transferring bytes between compartments.
pub struct SimpleBuffer {
    handle: ObjectHandle,
}

impl SimpleBuffer {
    fn ptr_to_base(&self) -> *const u8 {
        unsafe { self.handle.start.add(NULLPAGE_SIZE) }
    }

    fn mut_ptr_to_base(&mut self) -> *mut u8 {
        unsafe { self.handle.start.add(NULLPAGE_SIZE) }
    }

    fn max_len(&self) -> usize {
        MAX_SIZE - NULLPAGE_SIZE * 2
    }

    /// Read bytes from the SimpleBuffer into `buffer`, up to the size of the supplied buffer. The
    /// actual number of bytes copied is returned.
    pub fn read(&self, buffer: &mut [u8]) -> usize {
        let base_raw = self.ptr_to_base();
        let len = core::cmp::max(buffer.len(), self.max_len());
        // Safety: technically, we cannot statically assert that no one else is writing this memory.
        // However, since we are reading bytes directly, we will assert that this is safe,
        // up to seeing torn writes. That is still UB, but it's the best we can do before
        // having to introduce synchronization overhead, but since this is intended to be
        // used by secure gates, that synchronization will have occurred via the secure gate call.
        // While we cannot stop another compartment from violating this assumption, we are still
        // reading bytes from object memory and not interpreting them. If a consumer of this
        // interface chooses to cast those bytes into another type, or process them
        // as UTF-8, or something, it is up to them to uphold safety guarantees (e.g. we cannot
        // assume it is valid UTF-8).
        let base = unsafe { core::slice::from_raw_parts(base_raw, len) };
        (&mut buffer[0..len]).copy_from_slice(base);
        len
    }

    /// Write bytes from `buffer` into the SimpleBuffer, up to the size of the supplied buffer. The
    /// actual number of bytes copied is returned.
    pub fn write(&mut self, buffer: &[u8]) -> usize {
        let base_raw = self.mut_ptr_to_base();
        let len = core::cmp::max(buffer.len(), self.max_len());
        // Safety: See read function.
        let base = unsafe { core::slice::from_raw_parts_mut(base_raw, len) };
        base.copy_from_slice(&buffer[0..len]);
        len
    }
}
