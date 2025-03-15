use std::{io::ErrorKind, path::Path};

use secgate::util::{Handle, SimpleBuffer};
use twizzler::object::ObjID;
use twizzler_rt_abi::object::MapFlags;

use crate::{api::NamerAPI, NsNode, NsNodeKind, Result, PATH_MAX};

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
    fn write_buffer<P: AsRef<Path>>(&mut self, path: P) -> Result<usize> {
        let bytes = path.as_ref().as_os_str().as_encoded_bytes();
        if bytes.len() > PATH_MAX {
            Err(ErrorKind::InvalidFilename)
        } else {
            Ok(self.buffer.write(bytes))
        }
    }

    /// Open a new logging handle.
    pub fn new(api: &'a API) -> Option<Self> {
        NamingHandle::open(api).ok()
    }

    pub fn put<P: AsRef<Path>>(&mut self, path: P, id: ObjID) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api
            .put(self.desc, name_len, id, NsNodeKind::Object)
            .unwrap()
    }

    pub fn get(&mut self, path: &str) -> Result<ObjID> {
        let name_len = self.write_buffer(path)?;
        self.api.get(self.desc, name_len).unwrap().map(|n| n.id)
    }

    pub fn remove(&mut self, path: &str) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api.remove(self.desc, name_len).unwrap()
    }

    pub fn enumerate_names_nsid(&mut self, nsid: ObjID) -> Result<Vec<NsNode>> {
        let element_count = self.api.enumerate_names_nsid(self.desc, nsid).unwrap()?;

        let mut buf_vec = vec![0u8; element_count * std::mem::size_of::<NsNode>()];
        self.buffer.read(&mut buf_vec);
        let mut r_vec = Vec::new();

        for i in 0..element_count {
            unsafe {
                let entry_ptr = buf_vec
                    .as_ptr()
                    .offset((std::mem::size_of::<NsNode>() * i).try_into().unwrap())
                    as *const NsNode;
                r_vec.push(*entry_ptr);
            }
        }

        Ok(r_vec)
    }

    pub fn enumerate_names_relative(&mut self, path: &str) -> Result<Vec<NsNode>> {
        let name_len = self.write_buffer(path)?;
        let element_count = self.api.enumerate_names(self.desc, name_len).unwrap()?;

        let mut buf_vec = vec![0u8; element_count * std::mem::size_of::<NsNode>()];
        self.buffer.read(&mut buf_vec);
        let mut r_vec = Vec::new();

        for i in 0..element_count {
            unsafe {
                let entry_ptr = buf_vec
                    .as_ptr()
                    .offset((std::mem::size_of::<NsNode>() * i).try_into().unwrap())
                    as *const NsNode;
                r_vec.push(*entry_ptr);
            }
        }

        Ok(r_vec)
    }

    pub fn enumerate_names(&mut self) -> Result<Vec<NsNode>> {
        self.enumerate_names_relative(&".")
    }

    pub fn change_namespace(&mut self, path: &str) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api.change_namespace(self.desc, name_len).unwrap()
    }

    pub fn put_namespace(&mut self, path: &str) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api
            .put(self.desc, name_len, 0.into(), NsNodeKind::Namespace)
            .unwrap()
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
