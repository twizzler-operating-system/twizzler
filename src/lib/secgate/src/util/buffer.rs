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
}


#[cfg(kani)]
mod buffer {

    use twizzler_minruntime;
    use twizzler_rt_abi::bindings::twz_rt_map_object;
    use twizzler_abi::print_err;
    use twizzler_abi::syscall::{
        self, sys_object_create, BackingType, CreateTieSpec, LifetimeType, ObjectCreate, ObjectCreateFlags, Syscall,
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
            ),
            &[],
            &[],
        )
        .unwrap();

        twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE).unwrap()
    }


    fn raw_syscall_kani_stub(call: Syscall, args: &[u64]) -> (u64, u64) {

        // if core::intrinsics::unlikely(args.len() > 6) {
        //     twizzler_abi::print_err("too many arguments to raw_syscall");
        //     // crate::internal_abort();
        // }
        let a0 = *args.first().unwrap_or(&0u64);
        let a1 = *args.get(1).unwrap_or(&0u64);
        let mut a2 = *args.get(2).unwrap_or(&0u64);
        let a3 = *args.get(3).unwrap_or(&0u64);
        let a4 = *args.get(4).unwrap_or(&0u64);
        let a5 = *args.get(5).unwrap_or(&0u64);

        let mut num = call.num();
        //TODO: Skip actual inline assembly invcation and register inputs
        //TODO: Improve actual logic here

        (num,a2)
    }
    
    //TODO: Wrap fails at some point, unsure why
    #[kani::proof]
    #[kani::stub(twizzler_abi::arch::syscall::raw_syscall,raw_syscall_kani_stub)]
    #[kani::stub(twizzler_rt_abi::bindings::twz_rt_map_object, twizzler_minruntime::runtime::syms::twz_rt_map_object)]
    fn transfer() {
        let obj = new_handle();
        let mut sb = SimpleBuffer::new(obj);


        let bytes: [u8; 100] = kani::any();
        let wlen = sb.write(&bytes);
        let mut buf = [0u8; 100];

        assert_eq!(buf.len(), bytes.len());
        assert_eq!(buf.len(), wlen);

        let rlen = sb.read(&mut buf);
        assert_eq!(rlen, wlen);
        assert_eq!(&buf, &bytes);
    }

/// Test generated for harness `util::buffer::buffer::transfer`
///
/// Check for `assertion`: "This is a placeholder message; Kani doesn't support message formatted at runtime"
///
/// # Warning
///
/// Concrete playback tests combined with stubs or contracts is highly
/// experimental, and subject to change.
///
/// The original harness has stubs which are not applied to this test.
/// This may cause a mismatch of non-deterministic values if the stub
/// creates any non-deterministic value.
/// The execution path may also differ, which can be used to refine the stub
/// logic.

#[test]
fn kani_concrete_playback_transfer_3656479332704793845() {
    let concrete_vals: Vec<Vec<u8>> = vec![
    ];
    kani::concrete_playback_run(concrete_vals, transfer);
}
}

#[cfg(test)]
mod test {
    use twizzler_abi::syscall::{
        sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags,
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
