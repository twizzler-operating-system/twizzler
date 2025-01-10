#[link(name = "naming_srv")]
extern "C" {}

use naming_srv::{Schema, MAX_KEY_SIZE};
use secgate::util::{Descriptor, Handle, SimpleBuffer};
use twizzler_rt_abi::object::MapFlags;
use arrayvec::ArrayString;

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
        // I should write directly to the simple buffer
        let mut s = naming_srv::Schema { key: ArrayString::from(key).unwrap(), val };

        // Interpret schema as a slice
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };

        let handle = self.buffer.write(&bytes);

        naming_srv::put(self.desc);
    }

    pub fn get(&mut self, key: &str) -> Option<u128> {
        let mut s = naming_srv::Schema { key: ArrayString::from(key).unwrap(), val: 0 };
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };
        let handle = self.buffer.write(&bytes);

        naming_srv::get(self.desc).unwrap()
    }

    pub fn remove(&mut self, key: &str) {
        let mut s = naming_srv::Schema { key: ArrayString::from(key).unwrap(), val: 0 };
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };
        let handle = self.buffer.write(&bytes);

        naming_srv::remove(self.desc);
    }

    pub fn enumerate_names(&mut self) -> Vec<(String, u128)> {
        let element_count = naming_srv::enumerate_names(self.desc).unwrap().unwrap();

        let mut buf_vec = vec![0u8; element_count * std::mem::size_of::<Schema>()];
        self.buffer.read(&mut buf_vec);
        let buf_ptr = buf_vec.as_ptr();
        let mut r_vec = Vec::with_capacity(element_count);

        for i in 0..element_count {
            let schema = unsafe {
                let i_ptr = buf_ptr.byte_add(i * std::mem::size_of::<Schema>()) as *const Schema;
                *i_ptr
            };
            r_vec.push((schema.key.as_str().to_owned(), schema.val));
        }

        r_vec
    }
}
