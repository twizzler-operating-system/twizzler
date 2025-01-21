use arrayvec::ArrayString;
use secgate::util::{Handle, SimpleBuffer};
use twizzler_rt_abi::object::MapFlags;

use crate::{api::NamerAPI, definitions::Schema};
pub struct NamingHandle<'a, API: NamerAPI> {
    desc: u32,
    buffer: SimpleBuffer,
    api: &'a API,
}

impl<'a, API: NamerAPI> Drop for NamingHandle<'a, API> {
    fn drop(&mut self) {
        self.release();
    }
}

impl<'a, API: NamerAPI> NamingHandle<'a, API> {
    /// Open a new logging handle.
    pub fn new(api: &'a API) -> Option<Self> {
        NamingHandle::open(api).ok()
    }

    pub fn put(&mut self, key: &str, val: u128) {
        // I should write directly to the simple buffer
        let s = Schema {
            key: ArrayString::from(key).unwrap(),
            val,
        };

        // Interpret schema as a slice
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };

        let _handle = self.buffer.write(&bytes);

        self.api.put(self.desc);
    }

    pub fn get(&mut self, key: &str) -> Option<u128> {
        let s = Schema {
            key: ArrayString::from(key).unwrap(),
            val: 0,
        };
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };
        let _handle = self.buffer.write(&bytes);

        self.api.get(self.desc).unwrap()
    }

    pub fn remove(&mut self, key: &str) {
        let s = Schema {
            key: ArrayString::from(key).unwrap(),
            val: 0,
        };
        let bytes =
            unsafe { std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s) };
        let _handle = self.buffer.write(&bytes);

        self.api.remove(self.desc);
    }

    pub fn enumerate_names(&mut self) -> Vec<(String, u128)> {
        let element_count = self.api.enumerate_names(self.desc).unwrap().unwrap();

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

impl<'a, API: NamerAPI> Handle for NamingHandle<'a, API> {
    type OpenError = ();

    type OpenInfo = &'a API;

    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let (desc, id) = info.open_handle().ok().flatten().ok_or(())?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)
                .map_err(|_| ())?;
        let sb = SimpleBuffer::new(handle);
        Ok(Self {
            desc,
            buffer: sb,
            api: info,
        })
    }

    fn release(&mut self) {
        self.api.close_handle(self.desc);
    }
}
