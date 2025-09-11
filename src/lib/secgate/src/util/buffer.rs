use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::object::ObjectHandle;

/// A simple buffer to use for transferring bytes between compartments, using shared memory via
/// objects underneath.
pub struct SimpleBuffer {
    handle: ObjectHandle,
}

impl core::fmt::Debug for SimpleBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleBuffer")
            .field("id", &self.handle.id())
            .finish_non_exhaustive()
    }
}

impl SimpleBuffer {
    fn ptr_to_base(&self) -> *const u8 {
        unsafe { self.handle.start().add(NULLPAGE_SIZE) }
    }

    fn mut_ptr_to_base(&mut self) -> *mut u8 {
        unsafe { self.handle.start().add(NULLPAGE_SIZE) }
    }

    /// Build a new SimpleBuffer from an object handle.
    pub fn new(handle: ObjectHandle) -> Self {
        Self { handle }
    }

    /// Returns the maximum length of a read or write.
    pub fn max_len(&self) -> usize {
        MAX_SIZE - NULLPAGE_SIZE * 2
    }

    /// Get the underlying object handle.
    pub fn handle(&self) -> &ObjectHandle {
        &self.handle
    }

    pub fn into_handle(self) -> ObjectHandle {
        self.handle
    }

    /// Read bytes from the SimpleBuffer into `buffer`, up to the size of the supplied buffer. The
    /// actual number of bytes copied is returned.
    pub fn read(&self, buffer: &mut [u8]) -> usize {
        let base_raw = self.ptr_to_base();
        // Note that our len is not bounded by a previous write. But Twizzler objects are
        // 0-initialized by default, so all bytes are initialized to 0u8. If any other data _was_
        // written to the object, that still can be read as bytes.
        let len = core::cmp::min(buffer.len(), self.max_len());
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

    pub fn read_offset(&self, buffer: &mut [u8], offset: usize) -> usize {
        let base_raw = self.ptr_to_base();
        if offset >= self.max_len() {
            return 0;
        }
        let len = core::cmp::min(buffer.len(), self.max_len() - offset);
        let base = unsafe { core::slice::from_raw_parts(base_raw.add(offset), len) };
        (&mut buffer[0..len]).copy_from_slice(base);
        len
    }

    /// Write bytes from `buffer` into the SimpleBuffer, up to the size of the supplied buffer. The
    /// actual number of bytes copied is returned.
    pub fn write(&mut self, buffer: &[u8]) -> usize {
        let base_raw = self.mut_ptr_to_base();
        let len = core::cmp::min(buffer.len(), self.max_len());
        // Safety: See read function.
        let base = unsafe { core::slice::from_raw_parts_mut(base_raw, len) };
        base.copy_from_slice(&buffer[0..len]);
        len
    }

    /// Write bytes from `buffer` into the SimpleBuffer at provided offset, up to the size of the
    /// supplied buffer, minus the offset. The actual number of bytes copied is returned.
    pub fn write_offset(&mut self, buffer: &[u8], offset: usize) -> usize {
        let base_raw = self.mut_ptr_to_base();
        if offset >= self.max_len() {
            return 0;
        }
        let len = core::cmp::min(buffer.len(), self.max_len() - offset);
        // Safety: See read function.
        let base = unsafe { core::slice::from_raw_parts_mut(base_raw.add(offset), len) };
        base.copy_from_slice(&buffer[0..len]);
        len
    }
}

#[cfg(test)]
mod test {
    use twizzler_abi::{
        object::Protections,
        syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
    };
    use twizzler_rt_abi::object::{MapFlags, ObjectHandle};

    use super::*;

    fn new_handle() -> ObjectHandle {
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
                Protections::all(),
            ),
            &[],
            &[],
        )
        .unwrap();

        twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE).unwrap()
    }

    #[test]
    fn transfer() {
        let obj = new_handle();
        let mut sb = SimpleBuffer::new(obj);

        let data = b"simple buffer test!";
        let wlen = sb.write(data);
        let mut buf = [0u8; 19];
        assert_eq!(buf.len(), data.len());
        assert_eq!(buf.len(), wlen);

        let rlen = sb.read(&mut buf);
        assert_eq!(rlen, wlen);
        assert_eq!(&buf, data);
    }
}
