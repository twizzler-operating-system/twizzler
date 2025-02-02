use secgate::util::{Handle, SimpleBuffer};
use twizzler_rt_abi::object::MapFlags;

use crate::{api::NamerAPI, Entry, EntryType, ErrorKind, Result};

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

// TODO don't need seperate functions for names and namespaces?
impl<'a, API: NamerAPI> NamingHandle<'a, API> {
    /// Open a new logging handle.
    pub fn new(api: &'a API) -> Option<Self> {
        NamingHandle::open(api).ok()
    }

    pub fn put(&mut self, path: &str, val: u128) -> Result<()> {
        // I should write directly to the simple buffer
        let s = Entry::try_new(path, EntryType::Object(val))?;

        // Interpret Entry as a slice
        let bytes = unsafe { std::mem::transmute::<Entry, [u8; std::mem::size_of::<Entry>()]>(s) };

        let _handle = self.buffer.write(&bytes);

        self.api.put(self.desc).unwrap()
    }

    pub fn get(&mut self, path: &str) -> Result<u128> {
        let s = Entry::try_new(path, EntryType::Name)?; // Todo: Find better pattern to describe entries

        let bytes = unsafe { std::mem::transmute::<Entry, [u8; std::mem::size_of::<Entry>()]>(s) };
        let _handle = self.buffer.write(&bytes);

        match self.api.get(self.desc).unwrap()?.entry_type {
            EntryType::Object(x) => Ok(x),
            _ => Err(ErrorKind::NotNamespace),
        }
    }

    pub fn remove(&mut self, path: &str, recursive: bool) -> Result<()> {
        let s = Entry::try_new(path, EntryType::Namespace)?;

        let bytes = unsafe { std::mem::transmute::<Entry, [u8; std::mem::size_of::<Entry>()]>(s) };
        let _handle = self.buffer.write(&bytes);

        self.api.remove(self.desc, recursive).unwrap()
    }

    pub fn enumerate_names_relative(&mut self, path: &str) -> Result<Vec<Entry>> {
        let s = Entry::try_new(path, EntryType::Namespace)?;

        let bytes = unsafe { std::mem::transmute::<Entry, [u8; std::mem::size_of::<Entry>()]>(s) };
        let _handle = self.buffer.write(&bytes);

        let element_count = self.api.enumerate_names(self.desc).unwrap().unwrap();

        let mut buf_vec = vec![0u8; element_count * std::mem::size_of::<Entry>()];
        self.buffer.read(&mut buf_vec);
        let mut r_vec = Vec::new();

        for i in 0..element_count {
            unsafe {
                let entry_ptr = buf_vec
                    .as_ptr()
                    .offset((std::mem::size_of::<Entry>() * i).try_into().unwrap())
                    as *const Entry;
                r_vec.push(*entry_ptr);
            }
        }

        Ok(r_vec)
    }

    pub fn enumerate_names(&mut self) -> Result<Vec<Entry>> {
        self.enumerate_names_relative(&".")
    }

    pub fn change_namespace(&mut self, path: &str) -> Result<()> {
        let s = Entry::try_new(path, EntryType::Namespace)?;

        let bytes = unsafe { std::mem::transmute::<Entry, [u8; std::mem::size_of::<Entry>()]>(s) };
        let _handle = self.buffer.write(&bytes);

        self.api.change_namespace(self.desc).unwrap()
    }

    pub fn put_namespace(&mut self, path: &str) -> Result<()> {
        let s = Entry::try_new(path, EntryType::Namespace)?;
        let bytes = unsafe { std::mem::transmute::<Entry, [u8; std::mem::size_of::<Entry>()]>(s) };
        let _handle = self.buffer.write(&bytes);
        self.api.put(self.desc).unwrap()
    }

    pub fn get_namespace(&mut self, path: &str) -> Result<()> {
        let s = Entry::try_new(path, EntryType::Namespace)?;

        let bytes = unsafe { std::mem::transmute::<Entry, [u8; std::mem::size_of::<Entry>()]>(s) };
        let _handle = self.buffer.write(&bytes);

        match self.api.get(self.desc).unwrap()?.entry_type {
            EntryType::Namespace => Ok(()),
            _ => Err(ErrorKind::NotNamespace),
        }
    }

    pub fn get_working_namespace(&mut self) -> Result<Entry> {
        todo!()
    }
}

impl<'a, API: NamerAPI> Handle for NamingHandle<'a, API> {
    type OpenError = ();

    type OpenInfo = &'a API;

    fn open(info: Self::OpenInfo) -> std::result::Result<Self, Self::OpenError>
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
