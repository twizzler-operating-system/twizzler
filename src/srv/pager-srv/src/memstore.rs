use std::sync::Arc;

use object_store::{PagedObjectStore, PagingImp};
use twizzler::{object::ObjID, Result};
use twizzler_abi::{
    pager::{ObjectRange, PhysRange},
    syscall::{BackingType, LifetimeType},
};

use crate::helpers::PAGE;

pub mod virtio;

pub struct MemObjectInfo {
    lt: LifetimeType,
    bt: BackingType,
    len: u64,
}

pub trait MemStore {
    fn set_config_id(&self, id: ObjID) -> Result<()>;
    fn get_config_id(&self) -> Result<ObjID>;

    fn get_map(&self, id: ObjID, range: ObjectRange, phys: &mut Vec<(u64, u64)>) -> Result<()>;

    fn get_object_info(&self, id: ObjID) -> Result<MemObjectInfo>;

    fn flush(&self, id: Option<ObjID>) -> Result<()> {
        Ok(())
    }
}

pub trait MemDevice {
    fn page_size() -> u64
    where
        Self: Sized,
    {
        0x1000
    }

    fn get_physical(&self, start: u64, len: u64) -> Result<PhysRange>;

    fn flush(&self, start: u64, len: u64) -> Result<()> {
        Ok(())
    }
}

pub struct MemPageRequest {
    paddrs: Vec<(u64, u64)>,
    device: Arc<dyn MemDevice + Send + Sync + 'static>,
}

impl MemPageRequest {
    pub fn new(device: Arc<dyn MemDevice + Send + Sync + 'static>) -> Self {
        Self {
            device,
            paddrs: Vec::new(),
        }
    }
}

impl PagingImp for MemPageRequest {
    fn fill_from_buffer(&self, buf: &[u8]) {
        todo!()
    }

    fn read_to_buffer(&self, buf: &mut [u8]) {
        todo!()
    }

    fn phys_addrs(&self) -> &[(u64, u64)] {
        todo!()
    }

    fn page_size() -> usize
    where
        Self: Sized,
    {
        0x1000
    }

    fn page_in(&self, disk_pages: &[Option<u64>]) -> std::io::Result<usize> {
        for dp in disk_pages {
            if let Some(dp) = dp {
                let range = self.device.get_physical(*dp, PAGE)?;
                self.paddrs.push((range.start, range.len() as u64));
            } else {
            }
        }
        std::todo!()
    }

    fn page_out(&self, _disk_pages: &[Option<u64>]) -> std::io::Result<usize> {
        std::todo!()
    }

    fn phys_addrs_mut(&mut self) -> &mut Vec<(u64, u64)> {
        todo!()
    }
}

pub struct PagedMemStore {
    inner: Arc<dyn MemStore>,
}

impl PagedObjectStore for PagedMemStore {
    fn create_object(&self, id: object_store::ObjID) -> std::io::Result<()> {
        todo!()
    }

    fn delete_object(&self, id: object_store::ObjID) -> std::io::Result<()> {
        todo!()
    }

    fn len(&self, id: object_store::ObjID) -> std::io::Result<u64> {
        self.inner
            .get_object_info(id.into())
            .map(|i| i.len)
            .map_err(|e| e.into())
    }

    fn read_object(
        &self,
        id: object_store::ObjID,
        offset: u64,
        buf: &mut [u8],
    ) -> std::io::Result<usize> {
        todo!()
    }

    fn write_object(
        &self,
        id: object_store::ObjID,
        offset: u64,
        buf: &[u8],
    ) -> std::io::Result<()> {
        todo!()
    }

    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn page_in_object<'a>(
        &self,
        id: object_store::ObjID,
        reqs: &'a mut [object_store::PageRequest],
    ) -> std::io::Result<usize> {
        for req in reqs.iter_mut() {
            let range = ObjectRange {
                start: req.start_page as u64 * PAGE,
                end: (req.start_page as u64 + req.nr_pages as u64) * PAGE,
            };
            let paddrs = req.imp.phys_addrs_mut();
            self.inner.get_map(id.into(), range, paddrs)?;
        }

        Ok(reqs.len())
    }

    fn page_out_object<'a>(
        &self,
        id: object_store::ObjID,
        reqs: &'a [object_store::PageRequest],
    ) -> std::io::Result<usize> {
        Ok(reqs.len())
    }

    fn enumerate_external(
        &self,
        _id: object_store::ObjID,
    ) -> std::io::Result<Vec<object_store::ExternalFile>> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn find_external(&self, _id: object_store::ObjID) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn supplies_phys_addrs(&self) -> bool {
        true
    }
}
