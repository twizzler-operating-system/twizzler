#[link(name = "naming_srv")]
extern "C" {}

use naming_srv::{Schema, MAX_KEY_SIZE};
use secgate::util::{Descriptor, Handle, SimpleBuffer};
use twizzler_rt_abi::object::MapFlags;

pub struct NamingHandle {
    desc: Descriptor,
    buffer: SimpleBuffer,
}

impl Handle for NamingHandle {
    type OpenError = ();

    type OpenInfo = ();

    fn open(_info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let (desc, id) = naming_srv::open_handle().ok().flatten().ok_or(())?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)
                .map_err(|_| ())?;
        let sb = SimpleBuffer::new(handle);
        Ok(Self { desc, buffer: sb })
    }

    fn release(&mut self) {
        naming_srv::close_handle(self.desc);
    }
}

impl Drop for NamingHandle {
    fn drop(&mut self) {
        self.release()
    }
}

impl NamingHandle {
    /// Open a new logging handle.
    pub fn new() -> Option<Self> {
        Self::open(()).ok()
    }

    pub fn put(&mut self, key: &str, val: u128) {
        let mut buffer = [0u8; MAX_KEY_SIZE];
        let key_bytes = key.as_bytes();

        let length = key_bytes.len().min(MAX_KEY_SIZE);
        buffer[..length].copy_from_slice(&key_bytes[..length]);

        // I should write directly to the simple buffer
        let mut s = naming_srv::Schema { key: buffer, val };

        // Interpret schema as a slice
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };

        let handle = self.buffer.write(&bytes);

        naming_srv::put(self.desc);
    }

    pub fn get(&mut self, key: &str) -> Option<u128> {
        let mut buffer = [0u8; MAX_KEY_SIZE];
        let key_bytes = key.as_bytes();

        let length = key_bytes.len().min(MAX_KEY_SIZE);
        buffer[..length].copy_from_slice(&key_bytes[..length]);

        // I should write directly to the simple buffer
        let mut s = naming_srv::Schema {
            key: buffer,
            val: 0,
        };

        // Interpret schema as a slice
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };
        let handle = self.buffer.write(&bytes);

        naming_srv::get(self.desc).unwrap()
    }
}
