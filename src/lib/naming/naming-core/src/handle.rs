use std::path::Path;

use secgate::util::{Handle, SimpleBuffer};
use twizzler::object::ObjID;
use twizzler_rt_abi::{
    error::{ArgumentError, TwzError},
    object::MapFlags,
};

use crate::{api::NamerAPI, GetFlags, InlinePath, NsNode, Result, PATH_MAX};

pub struct NamingHandle<'a, API: NamerAPI> {
    desc: u32,
    /// Created on first use. A handle that only ever does `get` on short paths never makes one,
    /// which is what keeps a pool of handles from costing an object apiece.
    buffer: Option<SimpleBuffer>,
    api: &'a API,
}

impl<'a, API: NamerAPI> Drop for NamingHandle<'a, API> {
    fn drop(&mut self) {
        self.release();
    }
}

// TODO don't need seperate functions for names and namespaces?
impl<'a, API: NamerAPI> NamingHandle<'a, API> {
    /// The handle's shared buffer, asking the server to create it the first time.
    fn buffer(&mut self) -> Result<&mut SimpleBuffer> {
        if self.buffer.is_none() {
            let id = self.api.get_buffer(self.desc)?;
            let handle =
                twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)?;
            self.buffer = Some(SimpleBuffer::new(handle));
        }
        // Unwrap-Ok: just filled in above.
        Ok(self.buffer.as_mut().unwrap())
    }

    fn write_buffer<P: AsRef<Path>>(&mut self, path: P) -> Result<usize> {
        let bytes = path.as_ref().as_os_str().as_encoded_bytes();
        if bytes.len() > PATH_MAX {
            Err(ArgumentError::InvalidArgument.into())
        } else {
            Ok(self.buffer()?.write(bytes))
        }
    }

    fn write_buffer_at<P: AsRef<Path>>(&mut self, path: P, off: usize) -> Result<usize> {
        let bytes = path.as_ref().as_os_str().as_encoded_bytes();
        if bytes.len() > PATH_MAX {
            Err(ArgumentError::InvalidArgument.into())
        } else {
            Ok(self.buffer()?.write_offset(bytes, off))
        }
    }

    /// Open a new logging handle.
    pub fn new(api: &'a API) -> Option<Self> {
        NamingHandle::open(api).ok()
    }

    pub fn put<P: AsRef<Path>>(&mut self, path: P, id: ObjID) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api.put(self.desc, name_len, id)
    }

    pub fn get(&mut self, path: &str, flags: GetFlags) -> Result<NsNode> {
        if let Some(inline) = InlinePath::new(path) {
            return self.api.get_inline(self.desc, inline, flags);
        }
        let name_len = self.write_buffer(path)?;
        self.api.get(self.desc, name_len, flags)
    }

    pub fn remove(&mut self, path: &str) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api.remove(self.desc, name_len)
    }

    pub fn enumerate_names_nsid(
        &mut self,
        nsid: ObjID,
        skip: usize,
        count: usize,
    ) -> Result<Vec<NsNode>> {
        tracing::trace!(
            "enumerating namespace {} (skip {}, count {})",
            nsid,
            skip,
            count
        );
        // The server replies through the buffer, so it has to exist before the call.
        self.buffer()?;
        let element_count = self
            .api
            .enumerate_names_nsid(self.desc, nsid, skip, count)?;

        let mut buf_vec = vec![0u8; element_count * std::mem::size_of::<NsNode>()];
        self.buffer()?.read(&mut buf_vec);
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

    pub fn enumerate_names_relative<P: AsRef<Path>>(
        &mut self,
        path: P,
        skip: usize,
        count: usize,
    ) -> Result<Vec<NsNode>> {
        let name_len = self.write_buffer(path)?;
        let element_count = self.api.enumerate_names(self.desc, name_len, skip, count)?;

        let mut buf_vec = vec![0u8; element_count * std::mem::size_of::<NsNode>()];
        self.buffer()?.read(&mut buf_vec);
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

    pub fn enumerate_names(&mut self, skip: usize, count: usize) -> Result<Vec<NsNode>> {
        self.enumerate_names_relative(&".", skip, count)
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api.change_namespace(self.desc, name_len)
    }

    pub fn put_namespace<P: AsRef<Path>>(&mut self, path: P, persist: bool) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        self.api.mkns(self.desc, name_len, persist)
    }

    pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(&mut self, old: P, new: Q) -> Result<()> {
        let old_len = self.write_buffer(old)?;
        let new_len = self.write_buffer_at(new, old_len)?;
        self.api.rename(self.desc, old_len, new_len)
    }

    pub fn symlink<P: AsRef<Path>, L: AsRef<Path>>(&mut self, path: P, link: L) -> Result<()> {
        let name_len = self.write_buffer(path)?;
        let link_len = self.write_buffer_at(link, name_len)?;
        self.api.link(self.desc, name_len, link_len)
    }
}

impl<'a, API: NamerAPI> Handle for NamingHandle<'a, API> {
    type OpenError = TwzError;

    type OpenInfo = &'a API;

    fn open(info: Self::OpenInfo) -> std::result::Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let desc = info.open_handle()?;
        Ok(Self {
            desc,
            buffer: None,
            api: info,
        })
    }

    fn release(&mut self) {
        let _ = self
            .api
            .close_handle(self.desc)
            .inspect_err(|e| tracing::warn!("{}", e));
    }
}
